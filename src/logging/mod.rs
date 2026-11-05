mod path;
mod redact;
mod rotate;
mod timestamp;
mod writer;

use std::sync::OnceLock;

pub use redact::{
    AgentState, Authority, EventKind, ExitClass, Field, Level, Listener, LoggerKind, Outcome,
    Reason, Role, SafeId, SpawnKind, Worker,
};

use writer::Logger;

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub struct ProcessGuard {
    role: Role,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        event(
            match self.role {
                Role::Client => EventKind::ClientStop,
                Role::Server | Role::Local => EventKind::ServerStop,
            },
            &[],
        );
        shutdown();
    }
}

pub fn init(role: Role) -> ProcessGuard {
    let _ = LOGGER.get_or_init(|| Logger::start(role, configured_level()));
    ProcessGuard { role }
}

pub fn event(kind: EventKind, fields: &[Field]) {
    if let Some(logger) = LOGGER.get() {
        logger.event(kind, fields);
    }
}

pub fn shutdown() {
    if let Some(logger) = LOGGER.get() {
        logger.shutdown();
    }
}

pub(crate) fn resolved_dir() -> std::path::PathBuf {
    path::log_dir()
}

/// Return bounded event headers, never raw log fields or paths. Call off-loop.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn desktop_records(client: bool) -> std::io::Result<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;
    let root = crate::persist::session_dir();
    let pinned = crate::browser::WorkspaceFiles::open(&root)?;
    let path = std::path::Path::new(if client {
        "logs/client.log"
    } else {
        "logs/server.log"
    });
    let result = (|| {
        let (parent, name) = pinned.parent(path, &|| false)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.nlink() != 1
            || meta.mode() & 0o077 != 0
        {
            return Err(std::io::Error::other("unsafe log file"));
        }
        file.seek(SeekFrom::Start(meta.len().saturating_sub(64 * 1024)))?;
        let mut bytes = vec![];
        file.take(64 * 1024).read_to_end(&mut bytes)?;
        let mut rows = bytes
            .rsplit(|b| *b == b'\n')
            .filter_map(|line| {
                let v: serde_json::Value = serde_json::from_slice(line).ok()?;
                let ts = v["ts"].as_str()?;
                let level = v["level"].as_str()?;
                let event = v["event"].as_str()?;
                if [ts, level, event].iter().any(|s| {
                    s.is_empty()
                        || s.len() > 96
                        || !s
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"._-:+".contains(&c))
                }) {
                    return None;
                }
                Some(serde_json::json!({"time":ts,"level":level,"event":event}))
            })
            .take(100)
            .collect::<Vec<_>>();
        rows.reverse();
        Ok(serde_json::json!({"records":rows}))
    })();
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({"records":[]})),
        other => other,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn desktop_records(_: bool) -> std::io::Result<serde_json::Value> {
    Err(std::io::Error::other("secure log view unavailable"))
}

pub(crate) fn log_dir_writable() -> bool {
    path::directory_writable_without_creating(&resolved_dir())
}

fn configured_level() -> Option<Level> {
    let value = std::env::var("LUVUS_LOG").ok();
    configured_level_value(value.as_deref())
}

fn configured_level_value(value: Option<&str>) -> Option<Level> {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "off" => None,
        "error" => Some(Level::Error),
        "warn" => Some(Level::Warn),
        "debug" => Some(Level::Debug),
        "" | "info" => Some(Level::Info),
        _ => Some(Level::Info),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_level_uses_info_logging() {
        assert_eq!(configured_level_value(None), Some(Level::Info));
        assert_eq!(configured_level_value(Some("")), Some(Level::Info));
        assert_eq!(configured_level_value(Some("off")), None);
    }

    #[test]
    fn explicit_info_level_enables_logging() {
        assert_eq!(configured_level_value(Some("info")), Some(Level::Info));
        assert_eq!(configured_level_value(Some(" DEBUG ")), Some(Level::Debug));
    }

    #[test]
    fn unknown_level_uses_info_logging() {
        assert_eq!(configured_level_value(Some("surprise")), Some(Level::Info));
    }
}
