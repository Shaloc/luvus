//! Managed display SSH admission and multiplexing. Logical workspace streams
//! remain independent; owner-only cross-process leases bound channels per master.
use super::*;
use std::fs::File;

#[cfg(unix)]
const CHANNELS_PER_MASTER: usize = 8;
#[cfg(unix)]
const MAX_DISPLAY_CHANNELS: usize = 512;

/// Bounded probes, lists and lifecycle commands share a short-command master.
/// Keep it separate from leased display channels. Long installations bypass
/// this pool so they cannot block discovery or reconnection preflights.
pub(super) fn short_command(
    host: &str,
    command: Command,
    deadline: Instant,
) -> Result<(Command, Option<File>), String> {
    #[cfg(unix)]
    {
        let directory = host_directory(host).map_err(|error| error.to_string())?;
        // Real loopback SSH reproduced two concurrent initial probes opening
        // separate masters. Reuse bounded admission; only short commands wait
        // here, never display channels or the event loop.
        let admission = acquire_admission(
            &directory.join("commands.admission"),
            &ConnectionScope::default(),
            deadline,
            "SSH command admission timed out",
        )?;
        let logical = directory.join("commands.sock");
        let socket = crate::session::socket_alias_path(logical, "luvus", "ssh");
        crate::persist::ensure_private_server_dir(socket.parent().unwrap())
            .map_err(|error| error.to_string())?;
        // All callers pass ssh_base commands before adding stdio; retain the
        // original environment and working directory when prepending options.
        let mut shared = Command::new(command.get_program());
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                shared.env(key, value);
            } else {
                shared.env_remove(key);
            }
        }
        if let Some(directory) = command.get_current_dir() {
            shared.current_dir(directory);
        }
        multiplex(&mut shared, &socket);
        shared.args(command.get_args());
        Ok((shared, Some(admission)))
    }
    #[cfg(not(unix))]
    {
        let _ = (host, deadline);
        Ok((command, None))
    }
}

fn acquire_admission(
    path: &Path,
    scope: &ConnectionScope,
    deadline: Instant,
    timeout: &str,
) -> Result<File, String> {
    let handshake = private_file(path).map_err(|error| error.to_string())?;
    loop {
        if !scope.wait_for_retry(Duration::ZERO) {
            return Err("remote subscription closed".into());
        }
        match handshake.try_lock_exclusive() {
            Ok(()) => return Ok(handshake),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(timeout.into());
                }
                if !scope.wait_for_retry(Duration::from_millis(25)) {
                    return Err("remote subscription closed".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn multiplex(command: &mut Command, socket: &Path) {
    command
        .args(["-o", "ControlMaster=auto", "-o", "ControlPersist=30"])
        .arg("-S")
        .arg(socket.to_string_lossy().replace('%', "%%"));
}

pub(crate) struct DisplayConnection {
    handshake: Option<File>,
    _channel: Option<File>,
    socket: Option<PathBuf>,
}

impl DisplayConnection {
    pub(crate) fn acquire(host: &str, scope: &ConnectionScope) -> Result<Self, String> {
        let directory = host_directory(host).map_err(|e| e.to_string())?;
        let handshake = acquire_admission(
            &directory.join("handshake"),
            scope,
            Instant::now() + Duration::from_secs(120),
            "SSH display admission timed out",
        )?;
        #[allow(unused_mut)]
        let mut connection = Self {
            handshake: Some(handshake),
            _channel: None,
            socket: None,
        };
        // Windows OpenSSH does not implement Unix ControlMaster sockets. Keep
        // the same host admission bound and its existing independent transports.
        #[cfg(unix)]
        for slot in 0..MAX_DISPLAY_CHANNELS {
            let channel = private_file(&directory.join(format!("channel-{slot}")))
                .map_err(|e| e.to_string())?;
            match channel.try_lock_exclusive() {
                Ok(()) => {
                    let logical =
                        directory.join(format!("master-{}.sock", slot / CHANNELS_PER_MASTER));
                    let socket = crate::session::socket_alias_path(logical, "luvus", "ssh");
                    crate::persist::ensure_private_server_dir(socket.parent().unwrap())
                        .map_err(|e| e.to_string())?;
                    connection._channel = Some(channel);
                    connection.socket = Some(socket);
                    return Ok(connection);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        #[cfg(unix)]
        return Err("too many managed SSH display channels for this host".into());
        #[cfg(not(unix))]
        Ok(connection)
    }

    pub(crate) fn command(
        &self,
        target: &RemoteSession,
        location: RemoteBinaryLocation,
    ) -> Command {
        let base = bridge_command(target, "remote-client-bridge", location);
        if let Some(socket) = &self.socket {
            let mut command = Command::new(base.get_program());
            // Options precede the destination. A private socket plus channel
            // leases prevents other Luvus sessions from overfilling this master.
            multiplex(&mut command, socket);
            command.args(base.get_args());
            command
        } else {
            base
        }
    }

    pub(crate) fn authenticated(&mut self) {
        self.handshake.take();
    }
}

fn host_directory(host: &str) -> io::Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(host.as_bytes());
    let name = digest[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let base = crate::persist::config_dir().join("ssh-pool");
    crate::persist::ensure_private_server_dir(&base)?;
    let directory = base.join(name);
    crate::persist::ensure_private_server_dir(&directory)?;
    Ok(directory)
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH pool lease is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn installer_transport_does_not_wait_for_probe_admission() {
        let _env = crate::persist::test_env("ssh-installer-independent");
        let (_probe, _admission) = short_command(
            "fixture",
            ssh_base("fixture"),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        // The installer uses this bounded executor with a separate SSH
        // command. Holding probe admission must not delay its execution.
        let mut installer = Command::new("sh");
        installer.args(["-c", "printf installed"]);
        let output =
            run_ssh_command_with_timeout(installer, "fixture", 128, Duration::from_secs(1))
                .expect("installer must bypass short-command admission");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"installed");
    }

    #[test]
    fn short_commands_reuse_a_host_master_without_display_capacity() {
        let _env = crate::persist::test_env("ssh-short-command-pool");
        let socket = |command: &Command| {
            let args: Vec<_> = command.get_args().collect();
            args[args.iter().position(|arg| *arg == "-S").unwrap() + 1].to_owned()
        };
        let mut base = ssh_base("test-host");
        base.arg("luvus")
            .arg("--version")
            .env("POOL_TEST", "retained");
        base.current_dir(std::env::current_dir().unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        let (first, admission) = short_command("test-host", base, deadline).unwrap();
        let error = short_command("test-host", ssh_base("test-host"), Instant::now()).unwrap_err();
        assert!(error.contains("SSH command admission timed out"));
        let display = DisplayConnection::acquire("test-host", &ConnectionScope::default()).unwrap();
        assert_ne!(Path::new(&socket(&first)), display.socket.as_ref().unwrap());
        drop(admission);
        let (second, _admission) =
            short_command("test-host", ssh_base("test-host"), deadline).unwrap();
        assert_eq!(socket(&first), socket(&second));
        let (other, _other_admission) =
            short_command("other-host", ssh_base("other-host"), deadline).unwrap();
        assert_ne!(socket(&first), socket(&other));
        assert!(first
            .get_args()
            .any(|arg| arg == "StrictHostKeyChecking=yes"));
        assert_eq!(first.get_args().last().unwrap(), "--version");
        assert!(first
            .get_envs()
            .any(|(key, value)| key == "POOL_TEST" && value == Some("retained".as_ref())));
        assert_eq!(
            first.get_current_dir(),
            Some(std::env::current_dir().unwrap().as_path())
        );
    }

    #[test]
    fn pool_shards_display_channels_and_reuses_released_capacity() {
        let _env = crate::persist::test_env("ssh-pool-shards");
        let scope = ConnectionScope::default();
        let mut connections = Vec::new();
        for _ in 0..17 {
            let mut connection = DisplayConnection::acquire("test-host", &scope).unwrap();
            connection.authenticated();
            connections.push(connection);
        }
        assert_eq!(connections[0].socket, connections[7].socket);
        assert_ne!(connections[7].socket, connections[8].socket);
        assert_eq!(connections[8].socket, connections[15].socket);
        assert_ne!(connections[15].socket, connections[16].socket);
        let first = connections.remove(0).socket.clone();
        let replacement = DisplayConnection::acquire("test-host", &scope).unwrap();
        assert_eq!(replacement.socket, first);
        let command = replacement.command(
            &RemoteSession::new("test-host", "api").unwrap(),
            RemoteBinaryLocation::Path,
        );
        assert!(command.get_args().any(|arg| arg == "ControlMaster=auto"));
    }

    #[test]
    fn pool_admission_is_host_scoped_and_cancellation_wakes_waiters() {
        let _env = crate::persist::test_env("ssh-pool-cancel");
        let scope = Arc::new(ConnectionScope::default());
        let first = DisplayConnection::acquire("test-host", &scope).unwrap();
        let other = DisplayConnection::acquire("other-host", &scope).unwrap();
        let waiting = scope.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            tx.send(DisplayConnection::acquire("test-host", &waiting).is_err())
                .unwrap()
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        scope.cancel();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
        drop((first, other));
    }
}
