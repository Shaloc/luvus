//! Managed SSH hosts and remote server-session routing.
//!
//! Raw `--remote` remains the compatibility escape hatch. Managed remote
//! sessions and `--host` deliberately accept only literal `Host` aliases found
//! in the user's OpenSSH config, persist the alias (never its resolved
//! `HostName`), and require a compatible managed-session protocol before opening a
//! control or display bridge.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

mod input;
pub use input::RemoteInput;

pub const REMOTE_HOST_ENV_VAR: &str = "LUVUS_REMOTE_HOST";
pub const REMOTE_SESSION_ENV_VAR: &str = "LUVUS_REMOTE_SESSION";

const REGISTRY_VERSION: u32 = 1;
const MAX_SSH_CONFIG_FILES: usize = 128;
const MAX_SSH_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_VERSION_OUTPUT_BYTES: usize = 16 * 1024;
const SSH_CONNECT_TIMEOUT_SECONDS: &str = "10";
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

// Only the local presentation server sets this. PTYs and SSH children cannot
// inherit it, unlike a process environment selector.
static VIEW_TARGET: OnceLock<RemoteSession> = OnceLock::new();

pub fn set_view_target(target: RemoteSession) {
    let _ = VIEW_TARGET.set(target);
}

pub fn view_target() -> Option<&'static RemoteSession> {
    VIEW_TARGET.get()
}

/// Durable namespace identity, separate from snapshots (views intentionally
/// save no local workspaces). Never infer this identity from a name prefix.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SessionOrigin {
    Local,
    RemoteView { target: RemoteSession },
}

const SESSION_ORIGIN_FILE: &str = "session-origin.json";

/// Called only by the winning server while holding its existing startup lock.
/// An explicit owner-server start replaces a previous presentation identity.
pub(crate) fn record_session_origin(
    directory: &Path,
    target: Option<&RemoteSession>,
) -> io::Result<()> {
    let origin = match target {
        Some(target) => SessionOrigin::RemoteView {
            target: target.clone(),
        },
        None => SessionOrigin::Local,
    };
    write_remote_state_atomic(&directory.join(SESSION_ORIGIN_FILE), &origin)
}

pub(super) fn is_presentation_session(session: &super::SessionInfo) -> bool {
    let path = Path::new(&session.session_dir).join(SESSION_ORIGIN_FILE);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return false;
    };
    // Unknown, malformed or mismatched metadata must not hide a real session.
    if !metadata.is_file() || metadata.len() > 4096 {
        return false;
    }
    let origin = fs::File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, SessionOrigin>(file.take(4097)).ok());
    match origin {
        Some(SessionOrigin::RemoteView { target }) => {
            RemoteSession::new(&target.host, &target.session)
                .is_ok_and(|target| target.canonical_name() == session.name)
        }
        _ => false,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostSession {
    pub name: String,
    pub running: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostStatus {
    pub host: String,
    pub sessions: Vec<HostSession>,
    pub error: Option<String>,
}

/// User-triggered, bounded host discovery. Use the existing SSH preflight and
/// local named-session inventory, never recursive remote discovery or SCP.
pub fn discover_hosts() -> (RemoteRegistry, Vec<HostStatus>) {
    let mut registry = load_registry();
    let hosts = crate::config::load().remote_hosts;
    registry
        .sessions
        .retain(|target| hosts.contains(&target.host));
    let statuses: Vec<_> = hosts
        .chunks(4)
        .flat_map(|hosts| {
            thread::scope(|scope| {
                let jobs: Vec<_> = hosts
                    .iter()
                    .map(|host| {
                        scope.spawn(move || match list_host_sessions(host) {
                            Ok(sessions) => HostStatus {
                                host: host.clone(),
                                sessions,
                                error: None,
                            },
                            Err(error) => HostStatus {
                                host: host.clone(),
                                sessions: vec![],
                                error: Some(error),
                            },
                        })
                    })
                    .collect();
                jobs.into_iter()
                    .map(|job| job.join().expect("host discovery worker"))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for status in &statuses {
        if status.error.is_none() {
            registry
                .sessions
                .retain(|target| target.host != status.host);
            registry.sessions.extend(
                status
                    .sessions
                    .iter()
                    .filter_map(|session| RemoteSession::new(&status.host, &session.name).ok()),
            );
        }
    }
    registry.normalize();
    (registry, statuses)
}

pub fn list_host_sessions(host: &str) -> Result<Vec<HostSession>, String> {
    let location = verify_remote_version(host)?;
    let target = RemoteSession::new(host, super::DEFAULT_SESSION_NAME)?;
    let output = run_ssh_command(
        bridge_command(&target, "remote-session-list", location),
        host,
        1024 * 1024,
    )?;
    if !output.status.success() {
        return Err(remote_version_probe_failed(host, &output));
    }
    let sessions: Vec<HostSession> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("SSH host `{host}` returned an invalid session list: {error}"))?;
    if sessions.len() > 1024
        || sessions
            .iter()
            .any(|session| super::validate_name(&session.name).is_err())
    {
        return Err(format!(
            "SSH host `{host}` returned an invalid session inventory"
        ));
    }
    Ok(sessions)
}

/// Wait for the owner server to be ready before reporting session creation.
/// Merely spawning a control bridge is insufficient: dropping it cancels SSH.
pub fn ensure_session(target: &RemoteSession) -> Result<(), String> {
    let location = verify_remote_version(&target.host)?;
    let output = run_ssh_command(
        bridge_command(target, "remote-session-start", location),
        &target.host,
        MAX_VERSION_OUTPUT_BYTES,
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(remote_version_probe_failed(&target.host, &output))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct RemoteSession {
    pub host: String,
    pub session: String,
}

impl RemoteSession {
    pub fn new(host: &str, session: &str) -> Result<Self, String> {
        validate_host_alias(host)?;
        super::validate_name(session)?;
        let target = Self {
            host: host.to_string(),
            session: session.to_string(),
        };
        super::validate_name(&target.canonical_name()).map_err(|_| {
            "remote session name is longer than 64 bytes; shorten the SSH Host alias or session name"
                .to_string()
        })?;
        Ok(target)
    }

    pub fn canonical_name(&self) -> String {
        format!("remote-{}-{}", self.host, self.session)
    }
}

/// Resolve the local display namespace for an explicit remote open. Merge is
/// federation of existing owners, not authorization to create a local copy.
/// May inspect the selected home/sockets, so call only off the App event loop.
pub(crate) fn local_attach_name(target: &RemoteSession, merge: bool) -> Result<String, String> {
    RemoteSession::new(&target.host, &target.session)?;
    if merge && super::owner_session_exists(&target.session)? {
        Ok(target.session.clone())
    } else {
        let name = target.canonical_name();
        if super::owner_session_exists(&name)? {
            return Err(format!(
                "remote view `{name}` conflicts with an existing local session; choose a different remote session name"
            ));
        }
        Ok(name)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteRegistry {
    #[serde(default = "registry_version")]
    pub version: u32,
    #[serde(default)]
    pub sessions: Vec<RemoteSession>,
    #[serde(default)]
    pub merged_sessions: BTreeSet<String>,
    /// One installation-wide preference. None migrates the former per-name
    /// setting without overriding an explicit global off.
    #[serde(default)]
    pub merge: Option<bool>,
}

fn registry_version() -> u32 {
    REGISTRY_VERSION
}

impl RemoteRegistry {
    pub fn merge_enabled(&self) -> bool {
        self.merge.unwrap_or(!self.merged_sessions.is_empty())
    }

    fn normalize(&mut self) {
        self.version = REGISTRY_VERSION;
        self.merge = Some(self.merge_enabled());
        self.sessions
            .retain(|session| RemoteSession::new(&session.host, &session.session).is_ok());
        self.sessions
            .sort_by(|left, right| (&left.session, &left.host).cmp(&(&right.session, &right.host)));
        self.sessions.dedup();
        self.merged_sessions
            .retain(|name| super::validate_name(name).is_ok());
    }

    pub fn for_merge<'a>(
        &'a self,
        session: &'a str,
    ) -> impl Iterator<Item = &'a RemoteSession> + 'a {
        let enabled = self.merge_enabled();
        self.sessions
            .iter()
            .filter(move |target| enabled && target.session == session)
    }
}

pub fn registry_path() -> PathBuf {
    crate::persist::config_dir().join("remote-sessions.json")
}

fn registry_lock_path() -> PathBuf {
    crate::persist::config_dir().join("remote-sessions.lock")
}

pub fn load_registry() -> RemoteRegistry {
    let mut registry: RemoteRegistry = fs::read_to_string(registry_path())
        .ok()
        .and_then(|source| serde_json::from_str(&source).ok())
        .unwrap_or_default();
    registry.normalize();
    registry
}

pub fn add_session(target: RemoteSession, merge: bool) -> Result<RemoteRegistry, String> {
    mutate_registry(|registry| {
        if let Some(existing) = registry.sessions.iter().find(|existing| {
            existing.canonical_name() == target.canonical_name() && **existing != target
        }) {
            return Err(format!(
                "remote session name `{}` collides with host `{}` session `{}`; choose a different SSH alias or session name",
                target.canonical_name(), existing.host, existing.session
            ));
        }
        if !registry.sessions.contains(&target) {
            registry.sessions.push(target.clone());
        }
        if merge {
            registry.merge = Some(true);
        }
        Ok(())
    })
}

pub fn remove_session(name: &str) -> Result<RemoteRegistry, String> {
    mutate_registry(|registry| {
        let before = registry.sessions.len();
        let removed_session = registry
            .sessions
            .iter()
            .find(|target| target.canonical_name() == name)
            .map(|target| target.session.clone());
        registry
            .sessions
            .retain(|target| target.canonical_name() != name);
        if registry.sessions.len() == before {
            return Err(format!("remote session `{name}` is not registered"));
        }
        if let Some(session) = removed_session {
            if !registry
                .sessions
                .iter()
                .any(|target| target.session == session)
            {
                registry.merged_sessions.remove(&session);
            }
        }
        Ok(())
    })
}

pub fn set_merge(session: &str, enabled: bool) -> Result<RemoteRegistry, String> {
    super::validate_name(session)?;
    mutate_registry(|registry| {
        registry.merge = Some(enabled);
        registry.merged_sessions.clear();
        Ok(())
    })
}

/// User-triggered global preference changes update already-running local
/// namespaces, without starting servers or recursively contacting SSH hosts.
pub fn reload_local_sessions(exclude: Option<&str>) -> Result<bool, String> {
    let sessions = super::list_server_sessions().map_err(|error| error.to_string())?;
    let mut applied = false;
    for session in sessions
        .into_iter()
        .filter(|session| session.running && Some(session.name.as_str()) != exclude)
    {
        applied |= reload_local_session(&session.name)?;
    }
    Ok(applied)
}

#[cfg(test)]
mod global_merge_regression {
    #[test]
    fn merge_applies_to_every_same_name_and_survives_removing_a_registration() {
        let _env = crate::persist::test_env("global-remote-merge");
        super::add_session(super::RemoteSession::new("dev", "default").unwrap(), false).unwrap();
        super::add_session(super::RemoteSession::new("dev", "second").unwrap(), false).unwrap();
        let registry = super::set_merge("default", true).unwrap();
        assert_eq!(registry.for_merge("default").count(), 1);
        assert_eq!(registry.for_merge("second").count(), 1);
        let registry = super::remove_session("remote-dev-default").unwrap();
        assert_eq!(registry.for_merge("second").count(), 1);
        let registry = super::set_merge("second", false).unwrap();
        assert_eq!(registry.for_merge("default").count(), 0);
        assert_eq!(registry.for_merge("second").count(), 0);
    }
}

/// Reload the selected local server's existing configuration surface after a
/// registration change. An absent server reads the saved registry on startup.
/// Explicit paths avoid inherited pane/session selectors.
pub fn reload_local_session(session: &str) -> Result<bool, String> {
    let name = super::parse_target_name(session)?;
    let path = super::api_socket_path_for(name.as_deref());
    let timeout = Duration::from_secs(2);
    let mut connection = match crate::ipc::transport::connect_timeout(&path, timeout) {
        Ok(connection) => connection,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(false)
        }
        Err(error) => return Err(error.to_string()),
    };
    writeln!(
        connection,
        "{}",
        serde_json::json!({
            "id":"remote-registry-reload", "method":"server.reload_config", "params":{}
        })
    )
    .map_err(|error| error.to_string())?;
    let line = crate::ipc::api::read_response_frame_with_deadline(&mut connection, timeout)
        .map_err(|error| error.to_string())?;
    let response: serde_json::Value =
        serde_json::from_str(&line).map_err(|error| error.to_string())?;
    if let Some(error) = response.get("error") {
        return Err(error.to_string());
    }
    Ok(true)
}

fn mutate_registry(
    change: impl FnOnce(&mut RemoteRegistry) -> Result<(), String>,
) -> Result<RemoteRegistry, String> {
    fs::create_dir_all(crate::persist::config_dir()).map_err(|error| error.to_string())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(registry_lock_path())
        .map_err(|error| error.to_string())?;
    lock.lock_exclusive().map_err(|error| error.to_string())?;
    let mut registry = load_registry();
    change(&mut registry)?;
    registry.normalize();
    write_registry_atomic(&registry).map_err(|error| error.to_string())?;
    Ok(registry)
}

fn write_registry_atomic(registry: &RemoteRegistry) -> io::Result<()> {
    write_remote_state_atomic(&registry_path(), registry)
}

fn write_remote_state_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    static TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), id));
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        crate::platform::atomic_replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn resolve_canonical(name: &str) -> Result<Option<RemoteSession>, String> {
    if !name.starts_with("remote-") {
        return Ok(None);
    }
    let registry = load_registry();
    let mut candidates: Vec<_> = registry
        .sessions
        .iter()
        .filter(|target| target.canonical_name() == name)
        .cloned()
        .collect();
    if candidates.len() == 1 {
        return Ok(candidates.pop());
    }
    // Literal SSH aliases provide unambiguous boundaries even when host names
    // contain hyphens. Do not require prior registration of every session.
    for host in configured_hosts()? {
        if let Some(session) = name.strip_prefix(&format!("remote-{host}-")) {
            if let Ok(target) = RemoteSession::new(&host, session) {
                if !candidates.contains(&target) {
                    candidates.push(target);
                }
            }
        }
    }
    let Some(target) = candidates.first().cloned() else {
        return Err(format!(
            "remote session `{name}` does not match a literal SSH Host alias; select its host in Settings > Remote"
        ));
    };
    if candidates.len() > 1 {
        return Err(format!(
            "remote session `{name}` is ambiguous in the registry; remove one colliding host/session pair"
        ));
    }
    Ok(Some(target))
}

pub fn set_process_target(target: &RemoteSession) {
    std::env::set_var(REMOTE_HOST_ENV_VAR, &target.host);
    std::env::set_var(REMOTE_SESSION_ENV_VAR, &target.session);
}

pub fn clear_process_target() {
    std::env::remove_var(REMOTE_HOST_ENV_VAR);
    std::env::remove_var(REMOTE_SESSION_ENV_VAR);
}

pub fn process_target() -> Option<RemoteSession> {
    let host = std::env::var(REMOTE_HOST_ENV_VAR).ok()?;
    let session = std::env::var(REMOTE_SESSION_ENV_VAR).ok()?;
    RemoteSession::new(&host, &session).ok()
}

/// Strip a managed `--host` selector without consuming command payload. Leading
/// selectors are always global. A trailing selector is accepted for structured
/// commands, but pass-through/free-text commands require the leading form so a
/// prompt containing the literal words `--host foo` remains user data.
pub fn strip_host_selector(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    if args.is_empty() {
        return Ok((Vec::new(), None));
    }
    let payload_sensitive = command_payload_sensitive(args);
    let command_index = first_command_index(args);
    let command_boundary = command_index.unwrap_or(args.len());
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut cleaned = Vec::with_capacity(args.len());
    let mut host = None;
    let mut index = 0;
    while index < args.len() {
        // Before the noun, `--host` is unambiguously a global selector. After
        // the noun, accept it only as the final option pair (or final `=...`
        // option) of a structured command. This keeps an option value or free
        // text in the middle of a command from being silently stolen.
        let trailing_pair = !payload_sensitive && index + 2 == args.len();
        let trailing_equals = !payload_sensitive && index + 1 == args.len();
        let eligible = index < command_boundary
            || (index < separator
                && ((args[index] == "--host" && trailing_pair)
                    || (args[index].starts_with("--host=") && trailing_equals)));
        if eligible && args[index] == "--host" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| "missing value for --host".to_string())?;
            if host.replace(value.clone()).is_some() {
                return Err("--host may be passed only once".to_string());
            }
            index += 2;
            continue;
        }
        if eligible {
            if let Some(value) = args[index].strip_prefix("--host=") {
                if value.is_empty() {
                    return Err("missing value for --host".to_string());
                }
                if host.replace(value.to_string()).is_some() {
                    return Err("--host may be passed only once".to_string());
                }
                index += 1;
                continue;
            }
        }
        cleaned.push(args[index].clone());
        index += 1;
    }
    Ok((cleaned, host))
}

fn first_command_index(args: &[String]) -> Option<usize> {
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--session" | "--host" => index += 2,
            value if value.starts_with("--session=") || value.starts_with("--host=") => index += 1,
            _ => return Some(index),
        }
    }
    None
}

fn command_payload_sensitive(args: &[String]) -> bool {
    let Some(command) = first_command_index(args) else {
        return false;
    };
    matches!(
        (
            args.get(command).map(String::as_str),
            args.get(command + 1).map(String::as_str)
        ),
        (Some("search"), _)
            | (Some("workspace" | "node"), Some("rename"))
            | (
                Some("pane"),
                Some("run" | "send" | "name" | "report" | "report-event")
            )
            | (
                Some("agent"),
                Some("prompt" | "send" | "keys" | "name" | "report")
            )
            | (Some("tab"), Some("rename"))
            | (Some("task"), Some("add" | "update"))
            | (Some("ui"), Some("toast" | "dock" | "notification"))
            | (Some("bar"), Some("push"))
            | (Some("module"), Some("settings"))
            | (Some("diff"), Some("note"))
    )
}

pub fn validate_host_alias(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("SSH Host alias cannot be empty".to_string());
    }
    if host.starts_with('-') {
        return Err("SSH Host alias cannot start with `-`".to_string());
    }
    if !host
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "SSH Host alias may contain only ASCII letters, digits, `.`, `_`, and `-`".to_string(),
        );
    }
    Ok(())
}

pub fn configured_hosts() -> Result<Vec<String>, String> {
    let Some(home) = crate::platform::home_dir() else {
        return Err("could not locate the home directory for ~/.ssh/config".to_string());
    };
    configured_hosts_from(&home.join(".ssh/config"), &home)
}

pub fn enabled_hosts() -> Result<Vec<String>, String> {
    let selected = crate::config::load().remote_hosts;
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    Ok(configured_hosts()?
        .into_iter()
        .filter(|host| selected.contains(host))
        .collect())
}

pub fn require_enabled_host(host: &str) -> Result<(), String> {
    validate_host_alias(host)?;
    if !crate::config::load()
        .remote_hosts
        .iter()
        .any(|selected| selected == host)
    {
        return Err(format!(
            "SSH host `{host}` is not enabled in Luvus Settings > Remote; select it there before connecting"
        ));
    }
    require_configured_host(host)
}

fn configured_hosts_from(root: &Path, home: &Path) -> Result<Vec<String>, String> {
    let mut parser = SshConfigParser {
        home,
        visited: HashSet::new(),
        hosts: BTreeSet::new(),
        files: 0,
        bytes: 0,
    };
    parser.parse_file(root)?;
    Ok(parser.hosts.into_iter().collect())
}

pub fn require_configured_host(host: &str) -> Result<(), String> {
    validate_host_alias(host)?;
    if configured_hosts()?
        .iter()
        .any(|candidate| candidate == host)
    {
        Ok(())
    } else {
        Err(format!(
            "SSH host `{host}` is not declared as a literal Host in ~/.ssh/config"
        ))
    }
}

struct SshConfigParser<'a> {
    home: &'a Path,
    visited: HashSet<PathBuf>,
    hosts: BTreeSet<String>,
    files: usize,
    bytes: u64,
}

impl SshConfigParser<'_> {
    fn parse_file(&mut self, path: &Path) -> Result<(), String> {
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        if !self.visited.insert(canonical.clone()) {
            return Ok(());
        }
        self.files += 1;
        if self.files > MAX_SSH_CONFIG_FILES {
            return Err("SSH config includes too many files".to_string());
        }
        let metadata = fs::metadata(&canonical).map_err(|error| error.to_string())?;
        self.bytes = self.bytes.saturating_add(metadata.len());
        if self.bytes > MAX_SSH_CONFIG_BYTES {
            return Err("SSH config includes exceed the 4 MiB safety limit".to_string());
        }
        if !metadata.is_file() {
            return Err(format!(
                "SSH config {} is not a regular file",
                canonical.display()
            ));
        }
        let source = fs::read_to_string(&canonical)
            .map_err(|error| format!("could not read {}: {error}", canonical.display()))?;
        // OpenSSH resolves every relative user Include against ~/.ssh,
        // including directives nested in files below that directory.
        // https://man.openbsd.org/ssh_config#Include
        let base = self.home.join(".ssh");
        for line in source.lines() {
            let words = ssh_words(line)?;
            let Some(keyword) = words.first() else {
                continue;
            };
            if keyword.eq_ignore_ascii_case("host") {
                for alias in words.iter().skip(1) {
                    if !alias.starts_with('!')
                        && !alias
                            .bytes()
                            .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b']'))
                        && validate_host_alias(alias).is_ok()
                    {
                        self.hosts.insert(alias.clone());
                    }
                }
            } else if keyword.eq_ignore_ascii_case("include") {
                for include in words.iter().skip(1) {
                    for included in expand_include(include, &base, self.home)? {
                        self.parse_file(&included)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn ssh_words(line: &str) -> Result<Vec<String>, String> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return Ok(Vec::new());
    }
    let boundary = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(boundary);
    let rest = rest.trim_start();
    let line = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    let mut words = vec![keyword.to_string()];
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(expected) = quote {
            if character == expected {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '#' => break,
            c if c.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(character),
        }
    }
    if escaped {
        word.push('\\');
    }
    if quote.is_some() {
        return Err("unterminated quote in SSH config".to_string());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

fn expand_include(value: &str, base: &Path, home: &Path) -> Result<Vec<PathBuf>, String> {
    let expanded = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    if !expanded
        .as_os_str()
        .to_string_lossy()
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'['))
    {
        return Ok(vec![expanded]);
    }

    let mut paths = if expanded.is_absolute() {
        vec![PathBuf::from(std::path::MAIN_SEPARATOR.to_string())]
    } else {
        vec![PathBuf::new()]
    };
    for component in expanded.components() {
        let Component::Normal(component) = component else {
            if component == Component::ParentDir {
                for path in &mut paths {
                    path.pop();
                }
            }
            continue;
        };
        let pattern = component.to_string_lossy();
        if pattern
            .bytes()
            .any(|byte| matches!(byte, b'*' | b'?' | b'['))
        {
            let mut next = Vec::new();
            for parent in &paths {
                let entries = match fs::read_dir(parent) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                for entry in entries.flatten() {
                    if wildcard_match(&pattern, &entry.file_name().to_string_lossy()) {
                        next.push(entry.path());
                    }
                }
            }
            next.sort();
            paths = next;
        } else {
            for path in &mut paths {
                path.push(component);
            }
        }
    }
    Ok(paths)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    fn class_match(class: &[u8], actual: u8) -> bool {
        let (negated, class) = match class.first() {
            Some(b'!' | b'^') => (true, &class[1..]),
            _ => (false, class),
        };
        let mut matched = false;
        let mut index = 0;
        while index < class.len() {
            if index + 2 < class.len() && class[index + 1] == b'-' {
                matched |= class[index] <= actual && actual <= class[index + 2];
                index += 3;
            } else {
                matched |= class[index] == actual;
                index += 1;
            }
        }
        matched != negated
    }

    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((&b'*', rest)) => {
                inner(rest, value)
                    || value
                        .split_first()
                        .is_some_and(|(_, tail)| inner(pattern, tail))
            }
            Some((&b'?', rest)) => value
                .split_first()
                .is_some_and(|(_, tail)| inner(rest, tail)),
            Some((&b'[', rest)) => {
                let Some(end) = rest.iter().position(|byte| *byte == b']') else {
                    return value
                        .split_first()
                        .is_some_and(|(&actual, tail)| actual == b'[' && inner(rest, tail));
                };
                let (class, suffix) = rest.split_at(end);
                value.split_first().is_some_and(|(&actual, tail)| {
                    !class.is_empty() && class_match(class, actual) && inner(&suffix[1..], tail)
                })
            }
            Some((&expected, rest)) => value
                .split_first()
                .is_some_and(|(&actual, tail)| expected == actual && inner(rest, tail)),
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RemoteBinaryLocation {
    #[default]
    Path,
    StandardFallback,
}

pub fn verify_remote_version(host: &str) -> Result<RemoteBinaryLocation, String> {
    require_enabled_host(host)?;
    let primary = run_version_command(version_command(host, false), host)?;
    let primary_result = verified_version_output(host, &primary);
    if let Ok(Some(location)) = primary_result.as_ref() {
        return Ok(*location);
    }
    if !primary.status.success() && primary.status.code() != Some(127) {
        return Err(primary_result
            .err()
            .unwrap_or_else(|| remote_version_probe_failed(host, &primary)));
    }

    // A stale PATH entry must not hide a matching official user install. Try
    // the same bounded standard-path fallback used by the bridge itself.
    let fallback = run_version_command(version_command(host, true), host)?;
    let fallback_result = verified_version_output(host, &fallback);
    if let Ok(Some(location)) = fallback_result.as_ref() {
        return Ok(*location);
    }
    fallback_result?;
    primary_result?;
    Err(format!(
        "Luvus {} is required on SSH host `{host}`; install this modified version there (Luvus will not copy it automatically)",
        env!("CARGO_PKG_VERSION")
    ))
}

struct VersionOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_version_command(command: Command, host: &str) -> Result<VersionOutput, String> {
    run_ssh_command(command, host, MAX_VERSION_OUTPUT_BYTES)
}

fn run_ssh_command(
    mut command: Command,
    host: &str,
    limit: usize,
) -> Result<VersionOutput, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not launch ssh for `{host}`: {error}"))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("SSH version check has no stdout".to_string());
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("SSH version check has no stderr".to_string());
    };
    let stdout_reader = thread::spawn(move || drain_bounded(stdout, limit));
    let stderr_reader = thread::spawn(move || drain_bounded(stderr, MAX_VERSION_OUTPUT_BYTES));
    let deadline = Instant::now() + VERSION_CHECK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "timed out checking Luvus on SSH host `{host}` after {} seconds",
                    VERSION_CHECK_TIMEOUT.as_secs()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("could not wait for ssh to `{host}`: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "SSH stdout reader panicked".to_string())?
        .map_err(|error| error.to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "SSH stderr reader panicked".to_string())?
        .map_err(|error| error.to_string())?;
    Ok(VersionOutput {
        status: status?,
        stdout,
        stderr,
    })
}

pub(crate) fn drain_bounded(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(kept)
}

fn verified_version_output(
    host: &str,
    output: &VersionOutput,
) -> Result<Option<RemoteBinaryLocation>, String> {
    if !output.status.success() {
        if output.status.code() == Some(127) {
            return Ok(None);
        }
        return Err(remote_version_probe_failed(host, output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("luvus "))
    else {
        return Ok(None);
    };
    let mut fields = line.split_whitespace();
    let _name = fields.next();
    let Some(version) = fields.next() else {
        return Ok(None);
    };
    let remote_protocol = fields.clone().any(|field| field == "remote-session=2");
    let transport_protocol = fields.any(|field| {
        field
            .strip_prefix("transport=")
            .and_then(|version| version.parse().ok())
            .is_some_and(crate::ipc::protocol::supports_version)
    });
    if !remote_protocol || !transport_protocol {
        return Err(format!(
            "SSH host `{host}` has Luvus {version}, but it is not a compatible modified remote-session build; install a build supporting remote-session=2 and transport 9 or {} there (nothing will be installed automatically)",
            crate::ipc::protocol::PROTOCOL_VERSION
        ));
    }
    let fallback = String::from_utf8_lossy(&output.stderr).contains("LUVUS_STANDARD_FALLBACK");
    Ok(Some(if fallback {
        RemoteBinaryLocation::StandardFallback
    } else {
        RemoteBinaryLocation::Path
    }))
}

fn remote_version_probe_failed(host: &str, output: &VersionOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        format!(
            "could not check Luvus on SSH host `{host}` (ssh exited with {})",
            output.status
        )
    } else {
        format!("could not check Luvus on SSH host `{host}`: {detail}")
    }
}

fn ssh_base(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("NumberOfPasswordPrompts=0")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECONDS}"))
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg(host);
    command
}

/// Only failures that background work can repair belong in the retry loop.
/// SSH owns authentication and host-key approval; never answer either prompt.
pub(crate) fn failure_needs_attention(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "permission denied",
        "host key verification failed",
        "remote host identification has changed",
        "no matching host key",
        "no supported authentication",
        "too many authentication failures",
        "not a compatible modified",
        "protocol version mismatch",
        "projection protocol mismatch",
        "unsupported remote display",
        "install this modified",
        "not found in ssh config",
        "settings > remote",
    ]
    .iter()
    .any(|reason| error.contains(reason))
}

/// Bounded capture shared by control and display bridges. Keep draining after
/// the limit so verbose SSH diagnostics cannot deadlock either transport.
pub(crate) struct BridgeDiagnostics {
    done: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
}

impl BridgeDiagnostics {
    pub(crate) fn capture(stderr: impl Read + Send + 'static) -> Self {
        let (tx, done) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = tx.send(drain_bounded(stderr, MAX_VERSION_OUTPUT_BYTES));
        });
        Self { done }
    }

    pub(crate) fn failure(&self, fallback: impl std::fmt::Display) -> String {
        // EOF on stdout and stderr can be observed in either order. This is
        // failure-path worker IO only, never an app-loop wait.
        let detail = self
            .done
            .recv_timeout(Duration::from_millis(200))
            .ok()
            .and_then(Result::ok)
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                    .collect::<String>()
            })
            .unwrap_or_default();
        if detail.trim().is_empty() {
            fallback.to_string()
        } else {
            format!("SSH connection failed: {}", detail.trim())
        }
    }
}

fn version_command(host: &str, fallback: bool) -> Command {
    let mut command = ssh_base(host);
    if fallback {
        command.arg(standard_binary_script(&[
            "--version",
            "--remote-session-protocol",
        ]));
    } else {
        command
            .arg("luvus")
            .arg("--version")
            .arg("--remote-session-protocol");
    }
    command
}

fn standard_binary_script(args: &[&str]) -> String {
    let args = args
        .iter()
        .map(|argument| posix_shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "for luvus_bin in \"$HOME/.local/bin/luvus\" \"$HOME/.cargo/bin/luvus\" \
         \"$HOME/.nix-profile/bin/luvus\" /usr/local/bin/luvus /opt/homebrew/bin/luvus \
         /home/linuxbrew/.linuxbrew/bin/luvus; do if [ -x \"$luvus_bin\" ]; then \
         printf '%s\\n' LUVUS_STANDARD_FALLBACK >&2; exec \"$luvus_bin\" {args}; fi; done; exit 127"
    )
}

fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn bridge_command(
    target: &RemoteSession,
    role: &str,
    location: RemoteBinaryLocation,
) -> Command {
    // Managed controls, snapshots and frame subscriptions must never acquire
    // lifecycle authority. Explicit session open/create uses remote-session-start.
    let options: &[&str] = if matches!(role, "remote-control-bridge" | "remote-client-bridge") {
        &["--existing"]
    } else {
        &[]
    };
    bridge_command_with_options(target, role, location, options)
}

fn bridge_command_with_options(
    target: &RemoteSession,
    role: &str,
    location: RemoteBinaryLocation,
    options: &[&str],
) -> Command {
    let mut args = vec!["--session", &target.session, role];
    args.extend_from_slice(options);
    command_on_host(&target.host, &args, location)
}

fn command_on_host(host: &str, args: &[&str], location: RemoteBinaryLocation) -> Command {
    let mut command = ssh_base(host);
    match location {
        RemoteBinaryLocation::Path => {
            command.arg("luvus").args(args);
        }
        RemoteBinaryLocation::StandardFallback => {
            command.arg(standard_binary_script(args));
        }
    }
    command
}

/// Run one validated server-lifecycle command on the managed host. These
/// commands cannot travel through the control bridge they may stop, so they use
/// the same protocol-compatibility preflight and SSH policy as the bridge itself.
pub fn server_command(
    target: &RemoteSession,
    subcommand: &str,
    location: RemoteBinaryLocation,
    options: &[String],
) -> Command {
    debug_assert!(matches!(
        subcommand,
        "start" | "stop" | "restart" | "status" | "update-manifest"
    ));
    let mut command = ssh_base(&target.host);
    match location {
        RemoteBinaryLocation::Path => {
            command
                .arg("luvus")
                .arg("--session")
                .arg(&target.session)
                .arg("remote-server-command")
                .arg(subcommand)
                .args(options);
        }
        RemoteBinaryLocation::StandardFallback => {
            let mut args = vec![
                "--session",
                &target.session,
                "remote-server-command",
                subcommand,
            ];
            args.extend(options.iter().map(String::as_str));
            command.arg(standard_binary_script(&args));
        }
    }
    command
}

/// Stop the owner namespace from the session context menu, never its local
/// presentation namespace. Reuse the bounded SSH lifecycle transport.
pub fn stop_session(target: &RemoteSession) -> Result<(), String> {
    let location = verify_remote_version(&target.host)?;
    let output = run_ssh_command(
        server_command(target, "stop", location, &[]),
        &target.host,
        MAX_VERSION_OUTPUT_BYTES,
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not stop session `{}` on `{}`: {}",
            target.session,
            target.host,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// A confirmed UI deletion calls the owner's existing CLI, which rechecks
/// that the namespace is stopped. Do not select a managed namespace first:
/// a literal owner name beginning with remote- is not another SSH hop.
pub fn delete_session(target: &RemoteSession) -> Result<(), String> {
    RemoteSession::new(&target.host, &target.session)?;
    if target.session == super::DEFAULT_SESSION_NAME {
        return Err("deleting the default session is not supported".into());
    }
    let location = verify_remote_version(&target.host)?;
    let output = run_ssh_command(
        command_on_host(
            &target.host,
            &["session", "delete", &target.session],
            location,
        ),
        &target.host,
        MAX_VERSION_OUTPUT_BYTES,
    )?;
    if !output.status.success() {
        return Err(format!(
            "could not delete session `{}` on `{}`: {}",
            target.session,
            target.host,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // Forget only this cached handle. The next explicit discovery is still the
    // source of truth, and other same-name owners/host preferences are retained.
    mutate_registry(|registry| {
        registry.sessions.retain(|saved| saved != target);
        Ok(())
    })?;
    Ok(())
}

/// Cancellation for one user-selected merge subscription. Killing only its
/// SSH children wakes blocked readers; the worker owns reaping them.
#[derive(Default)]
pub(crate) struct ConnectionScope {
    state: Mutex<(bool, Vec<Weak<Mutex<Child>>>)>,
    cancelled: Condvar,
}

impl ConnectionScope {
    pub(crate) fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.0 = true;
            self.cancelled.notify_all();
            for child in state.1.drain(..).filter_map(|child| child.upgrade()) {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
            }
        }
    }

    /// Wait only after a connection failure; cancellation wakes a pending retry.
    pub(crate) fn wait_for_retry(&self, delay: Duration) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        self.cancelled
            .wait_timeout_while(state, delay, |state| !state.0)
            .is_ok_and(|(state, _)| !state.0)
    }

    pub(crate) fn register(&self, child: &Arc<Mutex<Child>>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "remote subscription closed")?;
        if state.0 {
            return Err("remote subscription closed".into());
        }
        state.1.retain(|child| child.strong_count() > 0);
        state.1.push(Arc::downgrade(child));
        Ok(())
    }
}

pub struct ControlConnection {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    diagnostics: BridgeDiagnostics,
}

impl Read for ControlConnection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match self.stdout.read(buffer) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                self.diagnostics.failure("SSH control bridge closed"),
            )),
            result => result,
        }
    }
}

impl Write for ControlConnection {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SSH input is closed"))?
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SSH input is closed"))?
            .flush()
    }
}

impl Drop for ControlConnection {
    fn drop(&mut self) {
        self.stdin.take();
        if let Ok(mut child) = self.child.lock() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

impl ControlConnection {
    pub(crate) fn in_scope(self, scope: &ConnectionScope) -> Result<Self, String> {
        scope.register(&self.child)?;
        Ok(self)
    }
}

pub fn connect_control(target: &RemoteSession) -> Result<ControlConnection, String> {
    let location = verify_remote_version(&target.host)?;
    connect_control_at(target, location)
}

/// One bounded request over the existing SSH control bridge, with lifecycle
/// startup/recovery disabled. Callers already
/// run off-loop; version negotiation retains its existing bounded preflight.
/// The response deadline cancels only this request's SSH child, never a server.
pub(crate) fn request_control(
    target: &RemoteSession,
    method: &str,
    params: serde_json::Value,
    timeout: Duration,
    response_limit: usize,
) -> Result<serde_json::Value, String> {
    let location = verify_remote_version(&target.host)?;
    let connection = spawn_control(bridge_command_with_options(
        target,
        "remote-control-bridge",
        location,
        &["--existing"],
    ))?;
    request_on_control(connection, method, params, timeout, response_limit)
}

fn request_on_control(
    connection: ControlConnection,
    method: &str,
    params: serde_json::Value,
    timeout: Duration,
    response_limit: usize,
) -> Result<serde_json::Value, String> {
    use std::io::BufRead;

    let scope = Arc::new(ConnectionScope::default());
    let mut connection = connection.in_scope(&scope)?;
    let request = serde_json::json!({"id":"remote-request","method":method,"params":params});
    let wire = format!("{request}\n");
    if wire.len() > response_limit {
        return Err("remote control request exceeds its byte limit".into());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = (|| {
            connection
                .write_all(wire.as_bytes())
                .map_err(|error| error.to_string())?;
            connection.flush().map_err(|error| error.to_string())?;
            let mut reader = io::BufReader::new(connection.take(response_limit as u64 + 1));
            let mut response = Vec::new();
            reader
                .read_until(b'\n', &mut response)
                .map_err(|error| error.to_string())?;
            if response.len() > response_limit {
                return Err("remote control response exceeds its byte limit".into());
            }
            if !response.ends_with(b"\n") {
                return Err("remote control response is incomplete".into());
            }
            serde_json::from_slice(&response)
                .map_err(|error| format!("invalid remote control response: {error}"))
        })();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => {
            scope.cancel();
            Err("remote control request timed out".into())
        }
    }
}

pub(crate) fn connect_control_at(
    target: &RemoteSession,
    location: RemoteBinaryLocation,
) -> Result<ControlConnection, String> {
    spawn_control(bridge_command(target, "remote-control-bridge", location))
}

fn spawn_control(mut command: Command) -> Result<ControlConnection, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not open SSH control bridge: {error}"))?;
    let Some(stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("SSH control bridge has no stdin".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err("SSH control bridge has no stdout".to_string());
    };
    let stderr = child.stderr.take().expect("piped SSH stderr");
    Ok(ControlConnection {
        child: Arc::new(Mutex::new(child)),
        stdin: Some(stdin),
        stdout,
        diagnostics: BridgeDiagnostics::capture(stderr),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_connection_failures_separate_attention_from_transient_outages() {
        for error in [
            "Permission denied (publickey)",
            "Host key verification failed.",
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
            "protocol version mismatch",
            "remote projection protocol mismatch: missing server generation",
            "not a compatible modified remote-session build",
        ] {
            assert!(failure_needs_attention(error), "{error}");
        }
        for error in [
            "Connection reset by peer",
            "Connection refused",
            "SSH bridge closed",
            "SSH display input write timed out",
            "remote server shut down",
        ] {
            assert!(!failure_needs_attention(error), "{error}");
        }
    }

    #[test]
    fn remote_bridge_diagnostics_preserve_actionable_ssh_error_without_control_bytes() {
        let diagnostics = BridgeDiagnostics::capture(io::Cursor::new(
            b"\x1b[31mPermission denied (publickey)\n\x07",
        ));
        let error = diagnostics.failure("EOF");
        assert!(error.contains("Permission denied (publickey)"));
        assert!(!error.contains('\x1b') && !error.contains('\x07'));
        assert!(failure_needs_attention(&error));
        let empty = BridgeDiagnostics::capture(io::empty());
        assert_eq!(empty.failure("Connection reset"), "Connection reset");
    }

    #[test]
    fn remote_retry_wait_is_woken_by_scope_cancellation() {
        let scope = Arc::new(ConnectionScope::default());
        assert!(scope.wait_for_retry(Duration::ZERO));
        let (started, ready) = std::sync::mpsc::channel();
        let (finished, result) = std::sync::mpsc::channel();
        let waiting = scope.clone();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            finished
                .send(waiting.wait_for_retry(Duration::from_secs(10)))
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        scope.cancel();
        assert!(!result.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
        assert!(!scope.wait_for_retry(Duration::ZERO));
    }

    #[test]
    fn managed_bridge_factories_have_no_implicit_server_lifecycle() {
        let target = RemoteSession::new("fake-dev", "api").unwrap();
        for location in [
            RemoteBinaryLocation::Path,
            RemoteBinaryLocation::StandardFallback,
        ] {
            for role in ["remote-control-bridge", "remote-client-bridge"] {
                let command = bridge_command(&target, role, location);
                let args = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(args.contains("--existing"), "{role}: {args}");
            }
            let command = bridge_command(&target, "remote-session-start", location);
            assert!(!command
                .get_args()
                .any(|arg| arg.to_string_lossy().contains("--existing")));
        }
    }

    #[cfg(unix)]
    fn fixture_control(script: &str) -> ControlConnection {
        let mut child = Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        ControlConnection {
            child: Arc::new(Mutex::new(child)),
            stdin,
            stdout,
            diagnostics: BridgeDiagnostics::capture(io::empty()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn one_shot_remote_control_stops_at_newline_and_reaps_its_own_child() {
        let connection = fixture_control(
            "IFS= read -r request; printf '{\"result\":{\"ok\":true}}\\n'; exec sleep 10",
        );
        let child = Arc::clone(&connection.child);
        let started = Instant::now();
        let result = request_on_control(
            connection,
            "ping",
            serde_json::json!({}),
            Duration::from_millis(300),
            1024,
        )
        .unwrap();
        assert_eq!(result["result"]["ok"], true);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn one_shot_remote_control_deadline_cancels_only_its_connection() {
        let connection = fixture_control("IFS= read -r request; exec sleep 10");
        let started = Instant::now();
        let result = request_on_control(
            connection,
            "ping",
            serde_json::json!({}),
            Duration::from_millis(20),
            1024,
        );
        assert!(result.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn one_shot_remote_control_rejects_oversized_response() {
        let connection = fixture_control("IFS= read -r request; printf '%0200d\\n' 0");
        let result = request_on_control(
            connection,
            "ping",
            serde_json::json!({}),
            Duration::from_millis(300),
            100,
        );
        assert!(result.unwrap_err().contains("response exceeds"));
    }

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn existing_only_control_option_reaches_both_binary_locations() {
        let target = RemoteSession::new("fake-dev", "api").unwrap();
        for location in [
            RemoteBinaryLocation::Path,
            RemoteBinaryLocation::StandardFallback,
        ] {
            let command = bridge_command_with_options(
                &target,
                "remote-control-bridge",
                location,
                &["--existing"],
            );
            let argv = command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>();
            assert!(argv.iter().any(|arg| arg.contains("--existing")));
            assert!(argv.iter().any(|arg| arg.contains("remote-control-bridge")));
        }
    }

    fn exit_status(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            Command::new("sh")
                .args(["-c", &format!("exit {code}")])
                .status()
                .unwrap()
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", &format!("exit {code}")])
                .status()
                .unwrap()
        }
    }

    #[test]
    fn host_selector_preserves_prompt_and_pass_through_payloads() {
        let (cleaned, host) = strip_host_selector(&strings(&[
            "luvus", "--host", "dev-207", "agent", "prompt", "reviewer", "keep", "--host",
            "literal",
        ]))
        .unwrap();
        assert_eq!(host.as_deref(), Some("dev-207"));
        assert_eq!(
            cleaned,
            strings(&["luvus", "agent", "prompt", "reviewer", "keep", "--host", "literal"])
        );

        let (cleaned, host) = strip_host_selector(&strings(&[
            "luvus", "pane", "run", "7", "tool", "--host", "literal",
        ]))
        .unwrap();
        assert_eq!(host, None);
        assert_eq!(
            cleaned,
            strings(&["luvus", "pane", "run", "7", "tool", "--host", "literal"])
        );
    }

    #[test]
    fn structured_commands_accept_trailing_host_selector() {
        let (cleaned, host) =
            strip_host_selector(&strings(&["luvus", "pane", "list", "--host", "dev-207"])).unwrap();
        assert_eq!(cleaned, strings(&["luvus", "pane", "list"]));
        assert_eq!(host.as_deref(), Some("dev-207"));
    }

    #[test]
    fn trailing_host_is_only_global_at_the_structured_command_edge() {
        let (cleaned, host) = strip_host_selector(&strings(&[
            "luvus", "pane", "list", "--host", "literal", "--json",
        ]))
        .unwrap();
        assert_eq!(host, None);
        assert_eq!(
            cleaned,
            strings(&["luvus", "pane", "list", "--host", "literal", "--json"])
        );

        let (cleaned, host) = strip_host_selector(&strings(&[
            "luvus",
            "workspace",
            "rename",
            "0",
            "keep",
            "--host",
            "literal",
        ]))
        .unwrap();
        assert_eq!(host, None);
        assert_eq!(
            cleaned,
            strings(&[
                "luvus",
                "workspace",
                "rename",
                "0",
                "keep",
                "--host",
                "literal",
            ])
        );
    }

    #[test]
    fn registry_resolves_ambiguous_hyphens_from_persisted_mapping() {
        let _environment = crate::persist::test_env("remote-registry-hyphens");
        let target = RemoteSession::new("dev-west-2", "api-blue").unwrap();
        add_session(target.clone(), false).unwrap();
        assert_eq!(
            resolve_canonical("remote-dev-west-2-api-blue").unwrap(),
            Some(target)
        );
    }

    #[test]
    fn registry_rejects_canonical_name_collisions() {
        let _environment = crate::persist::test_env("remote-registry-collision");
        add_session(RemoteSession::new("dev-west", "api").unwrap(), false).unwrap();
        let error = add_session(RemoteSession::new("dev", "west-api").unwrap(), false).unwrap_err();
        assert!(error.contains("collides"));
    }

    #[test]
    fn removing_the_last_same_name_remote_keeps_global_merge_preference() {
        let _environment = crate::persist::test_env("remote-registry-remove-merge");
        let first = RemoteSession::new("dev-a", "api").unwrap();
        let second = RemoteSession::new("dev-b", "api").unwrap();
        add_session(first.clone(), true).unwrap();
        add_session(second.clone(), false).unwrap();
        let registry = remove_session(&first.canonical_name()).unwrap();
        assert!(registry.merge_enabled());
        let registry = remove_session(&second.canonical_name()).unwrap();
        assert!(registry.merge_enabled());
    }

    #[test]
    fn ssh_config_lists_literal_hosts_and_follows_includes() {
        let _environment = crate::persist::test_env("remote-ssh-config");
        let home = crate::persist::config_dir().join("home");
        let ssh = home.join(".ssh");
        fs::create_dir_all(ssh.join("config.d")).unwrap();
        fs::write(
            ssh.join("config"),
            "Host dev-207 *.prod !blocked\n  HostName 11.162.237.207\nInclude config.d/*\n",
        )
        .unwrap();
        fs::write(
            ssh.join("config.d/team"),
            "Host = build-box staging_box\n  User developer\nInclude shared/*\n",
        )
        .unwrap();
        fs::create_dir_all(ssh.join("shared")).unwrap();
        fs::write(ssh.join("shared/hosts"), "Host=parent-include\n").unwrap();
        fs::write(ssh.join("config.d/team-a"), "Host bracket-host\n").unwrap();
        fs::write(ssh.join("config.d/team-z"), "Host excluded-bracket-host\n").unwrap();
        fs::write(
            ssh.join("config"),
            "Host dev-207 *.prod !blocked\n  HostName 11.162.237.207\nInclude config.d/team config.d/team-[a-b]\n",
        )
        .unwrap();
        assert_eq!(
            configured_hosts_from(&ssh.join("config"), &home).unwrap(),
            vec![
                "bracket-host",
                "build-box",
                "dev-207",
                "parent-include",
                "staging_box",
            ]
        );
    }

    #[test]
    fn canonical_remote_name_keeps_ssh_alias_not_hostname() {
        let target = RemoteSession::new("dev-207", "api").unwrap();
        assert_eq!(target.canonical_name(), "remote-dev-207-api");
    }

    #[test]
    fn managed_hosts_are_opt_in_and_rejected_before_any_ssh_probe() {
        let _environment = crate::persist::test_env("remote-hosts-opt-in");
        assert!(crate::config::load().remote_hosts.is_empty());
        assert!(enabled_hosts().unwrap().is_empty());
        let error = verify_remote_version("not-enabled").unwrap_err();
        assert!(error.contains("Settings > Remote"));
        assert!(RemoteSession::new("-oProxyCommand", "api").is_err());
    }

    #[test]
    fn compatible_remote_release_does_not_require_identical_package_version() {
        for transport in [9, crate::ipc::protocol::PROTOCOL_VERSION] {
            let output = VersionOutput {
                status: exit_status(0),
                stdout: format!("luvus 1.1.0 remote-session=2 transport={transport}\n")
                    .into_bytes(),
                stderr: Vec::new(),
            };
            assert_eq!(
                verified_version_output("build", &output).unwrap(),
                Some(RemoteBinaryLocation::Path)
            );
        }
    }

    #[test]
    fn managed_ssh_never_prompts_or_accepts_an_unknown_host_key() {
        let command = ssh_base("build");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        for option in [
            "BatchMode=yes",
            "NumberOfPasswordPrompts=0",
            "StrictHostKeyChecking=yes",
        ] {
            assert!(
                args.iter().any(|arg| arg == option),
                "missing {option}: {args:?}"
            );
        }
    }

    #[test]
    fn version_probe_requires_compatible_protocols_and_marks_fallbacks() {
        let matching_output = format!(
            "luvus {} remote-session=2 transport={}\n",
            env!("CARGO_PKG_VERSION"),
            crate::ipc::protocol::PROTOCOL_VERSION
        );
        let matching = VersionOutput {
            status: exit_status(0),
            stdout: matching_output.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(
            verified_version_output("dev-207", &matching).unwrap(),
            Some(RemoteBinaryLocation::Path)
        );

        let fallback = VersionOutput {
            status: exit_status(0),
            stdout: matching_output.into_bytes(),
            stderr: b"LUVUS_STANDARD_FALLBACK\n".to_vec(),
        };
        assert_eq!(
            verified_version_output("dev-207", &fallback).unwrap(),
            Some(RemoteBinaryLocation::StandardFallback)
        );

        let stale = VersionOutput {
            status: exit_status(0),
            stdout: b"luvus 0.13.4\n".to_vec(),
            stderr: Vec::new(),
        };
        assert!(verified_version_output("dev-207", &stale)
            .unwrap_err()
            .contains("not a compatible modified"));

        let unmodified = VersionOutput {
            status: exit_status(0),
            stdout: format!("luvus {}\n", env!("CARGO_PKG_VERSION")).into_bytes(),
            stderr: Vec::new(),
        };
        assert!(verified_version_output("dev-207", &unmodified)
            .unwrap_err()
            .contains("not a compatible modified"));
    }

    #[test]
    fn version_output_is_drained_but_retained_with_a_fixed_bound() {
        let bytes = vec![b'x'; MAX_VERSION_OUTPUT_BYTES * 2];
        assert_eq!(
            drain_bounded(std::io::Cursor::new(bytes), MAX_VERSION_OUTPUT_BYTES)
                .unwrap()
                .len(),
            MAX_VERSION_OUTPUT_BYTES
        );
    }

    #[test]
    fn remote_delete_argv_uses_literal_owner_name_without_managed_selector() {
        let args = ["session", "delete", "remote-build-review"];
        let command = command_on_host("build", &args, RemoteBinaryLocation::Path);
        let arguments = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            &arguments[arguments.len() - 5..],
            ["build", "luvus", "session", "delete", "remote-build-review"]
        );
        assert!(!arguments.iter().any(|a| a == "--session"));
        let command = command_on_host("build", &args, RemoteBinaryLocation::StandardFallback);
        let script = command.get_args().last().unwrap().to_string_lossy();
        assert!(script.contains("'session' 'delete' 'remote-build-review'"));
        assert!(!script.contains("--session"));
    }

    #[test]
    fn managed_server_commands_keep_the_remote_session_selector() {
        let target = RemoteSession::new("dev-207", "remote-api").unwrap();
        let command = server_command(&target, "status", RemoteBinaryLocation::Path, &[]);
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            &arguments[arguments.len() - 6..],
            [
                "dev-207",
                "luvus",
                "--session",
                "remote-api",
                "remote-server-command",
                "status"
            ]
        );
    }

    #[test]
    fn managed_restart_all_forwards_flags_in_both_binary_locations() {
        let target = RemoteSession::new("dev-207", "default").unwrap();
        let options = vec!["--all".to_string(), "--json".to_string()];
        for location in [
            RemoteBinaryLocation::Path,
            RemoteBinaryLocation::StandardFallback,
        ] {
            let command = server_command(&target, "restart", location, &options);
            let arguments = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let joined = arguments.join(" ");
            assert!(joined.contains("dev-207"));
            assert!(joined.contains("remote-server-command"));
            assert!(joined.contains("restart"));
            assert!(joined.contains("--all"));
            assert!(joined.contains("--json"));
        }
    }
}
