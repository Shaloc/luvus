//! Managed display SSH admission and multiplexing. Logical workspace streams
//! remain independent; owner-only cross-process leases bound channels per master.
use super::*;
use std::fs::File;

#[cfg(unix)]
const CHANNELS_PER_MASTER: usize = 8;
#[cfg(unix)]
const MAX_DISPLAY_CHANNELS: usize = 512;

pub(crate) struct DisplayConnection {
    handshake: Option<File>,
    _channel: Option<File>,
    socket: Option<PathBuf>,
}

impl DisplayConnection {
    pub(crate) fn acquire(host: &str, scope: &ConnectionScope) -> Result<Self, String> {
        let directory = host_directory(host).map_err(|e| e.to_string())?;
        let handshake = private_file(&directory.join("handshake")).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if !scope.wait_for_retry(Duration::ZERO) {
                return Err("remote subscription closed".into());
            }
            match handshake.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err("SSH display admission timed out".into());
                    }
                    // Only a waiting startup worker sleeps; cancellation wakes
                    // it immediately through the subscription's existing signal.
                    if !scope.wait_for_retry(Duration::from_millis(25)) {
                        return Err("remote subscription closed".into());
                    }
                }
                Err(error) => return Err(error.to_string()),
            }
        }
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
            command
                .args(["-o", "ControlMaster=auto", "-o", "ControlPersist=30"])
                .arg("-S")
                .arg(socket.to_string_lossy().replace('%', "%%"))
                .args(base.get_args());
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
