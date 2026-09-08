//! Explicit SSH host admission. Reuses the fork installer and the existing SSH
//! preflight/control inventory; never owns a remote server lifecycle.

use super::*;

const INSTALLER: &str = include_str!("../../../install.sh");

fn install_command(host: &str) -> Command {
    let mut command = ssh_base(host);
    // A reviewed embedded installer, not curl | sh from a mutable remote URL.
    // Remote uname chooses Darwin arm64 or Linux x86_64. Downloads come only
    // from this fork, are checksummed, and replace ~/.local/bin/luvus atomically.
    let script = format!("unset LUVUS_VERSION LUVUS_INSTALL_DIR;\n{INSTALLER}");
    command.arg(format!("sh -c {}", posix_shell_quote(&script)));
    command.stdin(Stdio::null());
    command
}

pub(crate) fn connect_host(host: &str, install: bool) -> Result<HostStatus, String> {
    match inspect_remote_build(host)? {
        RemoteBuild::Compatible(_) => {}
        RemoteBuild::NeedsInstall(reason) if !install => return Err(reason),
        RemoteBuild::NeedsInstall(_) => {
            // Deselecting a host while its read-only preflight ran revokes
            // admission before installation starts. Once admitted, an install
            // is a bounded operation; disconnect does not roll it back.
            require_enabled_host(host)?;
            let output = run_ssh_command_with_timeout(
                install_command(host),
                host,
                64 * 1024,
                Duration::from_secs(180),
            )?;
            if !output.status.success() {
                return Err(format!(
                    "Luvus installation failed on `{host}`: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            verify_remote_version(host)?;
        }
    }
    Ok(HostStatus {
        host: host.into(),
        sessions: list_host_sessions(host)?,
        error: None,
    })
}

pub(crate) fn add_host(host: &str, install: bool) -> Result<HostStatus, String> {
    require_configured_host(host)?;
    let mut config = crate::config::load();
    let baseline = config.clone();
    if !config.remote_hosts.iter().any(|selected| selected == host) {
        config.remote_hosts.push(host.into());
        config.remote_hosts.sort();
    }
    if !crate::config::save_changes_with_patch(
        &baseline,
        &config,
        Some(&serde_json::json!({"remote_hosts":config.remote_hosts})),
    ) {
        return Err("could not save SSH host selection".into());
    }
    let status = connect_host(host, install || config.remote_auto_install);
    // Preserve the selection on failure so Settings shows the connection error.
    // Reload existing local owners only; never start a default/local namespace.
    let reload = reload_local_sessions(None);
    let status = status?;
    reload?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_runs_embedded_fork_installer_without_server_commands() {
        let command = install_command("fixture");
        let args = command
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>();
        assert_eq!(command.get_program(), "ssh");
        assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
        let payload = args.last().unwrap();
        assert!(payload.contains("Shaloc/luvus"));
        assert!(!payload.contains("server restart"));
        assert!(!payload.contains("server start"));
        assert!(!payload.contains("scp "));
        assert!(payload.contains("SHA-256 mismatch"));
    }
}
