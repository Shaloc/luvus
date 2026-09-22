use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use super::super::types::IntegrationOperations;
use crate::integration::{self, ShellHookSpec};

#[cfg(unix)]
const LAUNCHER: &str = include_str!("launch.py");

fn hook_command() -> String {
    let path = config_dir().join("luvus-agent-hook.sh");
    #[cfg(unix)]
    return format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    #[cfg(not(unix))]
    path.to_string_lossy().into_owned()
}

fn hook_script() -> String {
    let script = integration::agent_hook_script("codex");
    #[cfg(unix)]
    let script = script.replacen(
        "# luvus",
        "# A shared Codex daemon may inherit another pane's environment.\n\
         # Only a session-scoped launcher command supplies a valid route.\n\
         [ \"${LUVUS_CODEX_HOOK_CONTEXT:-}\" = \"1\" ] || exit 0\n# luvus",
        1,
    );
    script
}

fn is_hook_command(value: &Value) -> bool {
    let Some(command) = value.as_str() else {
        return false;
    };
    if command == hook_command() {
        return true;
    }
    // Older installations used a bare path. Keep recognizing it when no shell
    // quoting was necessary, without accepting arbitrary commands by substring.
    let path = config_dir().join("luvus-agent-hook.sh");
    let path = path.to_string_lossy();
    command == path
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
}

fn executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

pub(super) const OPERATIONS: IntegrationOperations = IntegrationOperations {
    install,
    uninstall,
    is_installed,
    hook: None,
};

fn config_dir() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| integration::home().join(".codex"))
}

fn spec() -> ShellHookSpec {
    ShellHookSpec {
        dir: config_dir(),
        file: "hooks.json",
        event: "SessionStart",
        matcher: Some("startup|resume"),
    }
}

fn install() -> Result<()> {
    #[cfg(unix)]
    let launcher = integration::launcher_dir().join("codex");
    #[cfg(unix)]
    if launcher.exists()
        && !fs::read_to_string(&launcher)?
            .starts_with("#!/usr/bin/env python3\n# Luvus Codex launcher:")
    {
        anyhow::bail!("Codex integration launcher already contains unmanaged content");
    }
    let dir = integration::install_shell_hook_with_spec("codex", spec())?;
    let config = dir.join("hooks.json");
    let script = dir.join("luvus-agent-hook.sh");
    integration::write_bytes_atomic(&script, hook_script().as_bytes())?;
    integration::set_executable(&script)?;
    #[cfg(unix)]
    {
        fs::create_dir_all(integration::launcher_dir())?;
        integration::write_bytes_atomic(&launcher, LAUNCHER.as_bytes())?;
        integration::set_executable(&launcher)?;
    }
    let mut value: Value = serde_json::from_slice(&fs::read(&config)?)?;
    integration::register_hook(
        &mut value,
        "SessionStart",
        Some("startup|resume"),
        &hook_command(),
        Some(5),
    );
    integration::register_hook(
        &mut value,
        "UserPromptSubmit",
        None,
        &hook_command(),
        Some(5),
    );
    integration::write_json_atomic(&config, &value)?;
    Ok(())
}

fn uninstall() -> Result<()> {
    integration::uninstall_shell_hook(spec(), &["UserPromptSubmit"])?;
    #[cfg(unix)]
    {
        let launcher = integration::launcher_dir().join("codex");
        if fs::read_to_string(&launcher).is_ok_and(|contents| contents == LAUNCHER) {
            fs::remove_file(launcher)?;
        }
    }
    Ok(())
}

fn is_installed() -> bool {
    let script = config_dir().join("luvus-agent-hook.sh");
    if !executable_file(&script)
        || !fs::read_to_string(&script).is_ok_and(|contents| contents == hook_script())
    {
        return false;
    }
    #[cfg(unix)]
    {
        let launcher = integration::launcher_dir().join("codex");
        if !executable_file(&launcher)
            || !fs::read_to_string(launcher).is_ok_and(|contents| contents == LAUNCHER)
        {
            return false;
        }
    }
    let Ok(contents) = fs::read(config_dir().join("hooks.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&contents) else {
        return false;
    };
    ["SessionStart", "UserPromptSubmit"].iter().all(|event| {
        value["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|group| {
                (*event != "SessionStart" || group["matcher"] == "startup|resume")
                    && group["hooks"].as_array().is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook["type"] == "command" && is_hook_command(&hook["command"])
                        })
                    })
            })
        })
    })
}
