//! Display-owned browser networking and Chrome sessions. Only local SSH dynamic
//! forwarding is used; the owner never runs a browser or opens a proxy listener.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub(crate) mod cdp;
pub mod control;
mod files;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use files::WorkspaceFiles;
pub mod proxy;

const MAX_SSH_CONFIG_BYTES: usize = 64 * 1024;
pub const BLANK_URL: &str = "about:blank";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub url: String,
    pub proxied: bool,
    /// Added only by an accepted remote projection, never by browser.open/panel.
    pub hosts: Vec<String>,
    pub origin: Option<u64>,
}

/// Transport 20 extends the frozen transport-19 request without changing its
/// encoding. Only the owning server captures the environment; relays preserve it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LaunchRequest {
    pub request: Request,
    pub proxy: Option<proxy::Environment>,
}

impl LaunchRequest {
    pub fn owner(request: Request) -> Self {
        Self {
            request,
            proxy: Some(proxy::Environment::capture()),
        }
    }
}

impl From<Request> for LaunchRequest {
    fn from(request: Request) -> Self {
        Self {
            request,
            proxy: None,
        }
    }
}

pub fn valid_url(url: &str) -> bool {
    url.len() <= 8192 && crate::platform::is_openable_url(url)
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_url(&self.url) && !(self.proxied && self.url == BLANK_URL) {
            return Err("expected an HTTP(S) browser URL".into());
        }
        // An alias defined only on an intermediate machine is not a route on
        // the display host. Require a direct local SSH alias (which may itself
        // use ProxyJump) instead of silently opening the wrong localhost.
        if self.hosts.len() > 1 {
            return Err("Browser sessions need a direct display-side SSH alias; configure ProxyJump and attach through that alias".into());
        }
        for host in &self.hosts {
            crate::session::remote::validate_host_alias(host)?;
        }
        Ok(())
    }
}

struct Session {
    stop: mpsc::Sender<()>,
    finished: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
pub struct Sessions(Vec<Session>);

impl Sessions {
    /// All discovery, SSH and process work happens on a user-triggered worker.
    pub fn open(
        &mut self,
        request: LaunchRequest,
        report: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        self.0.retain(|s| !s.finished.load(Ordering::Acquire));
        if self.0.len() >= 8 {
            report(Err(
                "Close a browser session before opening another (limit 8)".into(),
            ));
            return;
        }
        let (tx, rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let stop = tx.clone();
        let worker_finished = finished.clone();
        let worker = std::thread::spawn(move || {
            match launch(&request, &tx, &rx) {
                Ok(resources) => {
                    report(Ok(()));
                    let _ = rx.recv(); // browser exit, SSH exit, or display detach
                    drop(resources);
                }
                Err(error) => report(Err(error)),
            }
            worker_finished.store(true, Ordering::Release);
        });
        self.0.push(Session {
            stop,
            finished,
            worker: Some(worker),
        });
    }
}

/// An explicitly requested embedded browser's display-owned network lifetime.
/// Only the numeric loopback HTTP port leaves Rust; proxy policy and credentials
/// remain in the forwarding worker. No Chrome discovery or profile is involved.
#[allow(dead_code)] // The same-binary app adapter consumes this independently of Sessions.
pub(crate) struct NetworkLease {
    port: u16,
    stop: mpsc::Sender<()>,
    closed: Option<mpsc::Receiver<Result<(), String>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[allow(dead_code)]
impl NetworkLease {
    /// Call on a user-triggered worker, not the app loop. `stop` and `cancelled`
    /// must be the sender and receiver of the same channel (as in Sessions).
    /// Retain a sender clone to cancel startup or revoke a ready lease by sending
    /// `()`. Merely dropping that clone is not cancellation; dropping the lease is.
    ///
    /// Success means the loopback HTTP listener and any SSH SOCKS connection are
    /// ready. Even an empty or absent legacy policy uses the HTTP forwarder, never
    /// the display's environment or a Chromium DIRECT/system-proxy fallback.
    pub(crate) fn open(
        launch: &LaunchRequest,
        stop: mpsc::Sender<()>,
        cancelled: mpsc::Receiver<()>,
    ) -> Result<Self, String> {
        validate_launch(launch)?;
        check_cancelled(&cancelled)?;
        let launch = launch.clone();
        let done = stop.clone();
        let (ready, readiness) = mpsc::sync_channel(1);
        let (closed, notification) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("browser-network".into())
            .spawn(move || {
                // Create and retain platform child guards on this thread. In
                // particular, Windows Job Object guards are not Send.
                match Connection::open(&launch, &done, &cancelled, true) {
                    Ok(connection) => {
                        let port = connection
                            .forwarder
                            .as_ref()
                            .map(|forwarder| forwarder.port)
                            .ok_or_else(|| {
                                "Embedded browser requires an HTTP forwarder".to_string()
                            });
                        if ready.send(port).is_ok() {
                            let result = Self::wait_closed(connection, &cancelled);
                            let _ = closed.send(result);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|_| "Cannot start browser network worker".to_string())?;
        let mut lease = Self {
            port: 0,
            stop,
            closed: Some(notification),
            worker: Some(worker),
        };
        // Keep the stop/join guard even when startup fails or the worker panics.
        lease.port = readiness
            .recv()
            .map_err(|_| "Browser network worker closed before startup".to_string())??;
        Ok(lease)
    }

    fn wait_closed(connection: Connection, cancelled: &mpsc::Receiver<()>) -> Result<(), String> {
        let _ = cancelled.recv(); // explicit cancellation, SSH exit, or lease drop
        let result = if connection.children.exited.load(Ordering::Acquire) {
            Err("SSH browser connection closed".into())
        } else {
            Ok(())
        };
        drop(connection);
        // Notify only after listener/tunnels have been revoked.
        result
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// One terminal notification: Ok for cancellation/drop, Err for SSH exit.
    /// Startup failures are returned by open. A disconnected notification channel
    /// must also be treated as closure. Per-request proxy failures remain HTTP 502.
    pub(crate) fn take_closed(&mut self) -> Option<mpsc::Receiver<Result<(), String>>> {
        self.closed.take()
    }
}

impl Drop for NetworkLease {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Children {
    entries: Vec<(crate::platform::ChildTreeGuard, mpsc::Receiver<()>)>,
    exited: Arc<AtomicBool>,
}

impl Children {
    fn terminate(&mut self) {
        for (child, _) in &mut self.entries {
            child.refresh_descendants();
            child.terminate();
        }
    }

    fn reap(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut reaped = true;
        for (_, done) in self.entries.drain(..) {
            reaped &= done
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok();
        }
        reaped
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        self.terminate();
        self.reap();
    }
}

#[derive(Default)]
struct Connection {
    children: Children,
    forwarder: Option<proxy::Forwarder>,
    socks_port: Option<u16>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        drop(self.forwarder.take());
        // Children owns both termination and bounded reaping, including errors
        // during SOCKS readiness or forwarding-policy initialization.
    }
}

struct Resources {
    children: Children,
    profile: PathBuf,
    connection: Connection,
}

impl Drop for Resources {
    fn drop(&mut self) {
        drop(self.connection.forwarder.take());
        self.connection.children.terminate();
        self.children.terminate();
        let network_reaped = self.connection.children.reap();
        let reaped = self.children.reap();
        if network_reaped && reaped {
            let _ = std::fs::remove_dir_all(&self.profile);
        }
    }
}

fn watch_child(
    mut command: Command,
    children: &mut Children,
    done: &mpsc::Sender<()>,
) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::no_window(&mut command);
    crate::platform::suspend_for_child_tree(&mut command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let guard = match crate::platform::ChildTreeGuard::attach(&mut child) {
        Ok(guard) => guard,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
    };
    let (reaped, wait) = mpsc::channel();
    children.entries.push((guard, wait));
    let exited = children.exited.clone();
    let done = done.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        exited.store(true, Ordering::Release);
        let _ = reaped.send(());
        let _ = done.send(());
    });
    Ok(())
}

fn validate_launch(launch: &LaunchRequest) -> Result<(), String> {
    launch.request.validate()?;
    if let Some(env) = &launch.proxy {
        env.validate()?;
    }
    Ok(())
}

fn check_cancelled(cancelled: &mpsc::Receiver<()>) -> Result<(), String> {
    match cancelled.try_recv() {
        Err(mpsc::TryRecvError::Empty) => Ok(()),
        _ => Err("Browser request cancelled".into()),
    }
}

impl Connection {
    fn open(
        launch: &LaunchRequest,
        done: &mpsc::Sender<()>,
        cancelled: &mpsc::Receiver<()>,
        embedded: bool,
    ) -> Result<Self, String> {
        let mut connection = Self::default();
        connection.prepare(launch, done, cancelled, embedded)?;
        Ok(connection)
    }

    fn prepare(
        &mut self,
        launch: &LaunchRequest,
        done: &mpsc::Sender<()>,
        cancelled: &mpsc::Receiver<()>,
        embedded: bool,
    ) -> Result<(), String> {
        validate_launch(launch)?;
        check_cancelled(cancelled)?;
        if let Some(host) = launch.request.hosts.first() {
            // Check the same effective options before opening the connection. A
            // configured RemoteForward is forbidden, including inherited defaults.
            let probe = insert_ssh_option(ssh_command(host, None), "-G");
            let output = crate::session::remote::run_ssh_command_with_timeout(
                probe,
                host,
                MAX_SSH_CONFIG_BYTES + 1,
                Duration::from_secs(10),
            )?;
            // The shared probe has a bounded wait. Never spawn the tunnel if the
            // display detached while that probe was resolving its configuration.
            check_cancelled(cancelled)?;
            if !output.status.success() {
                return Err("Could not resolve the display-side SSH configuration".into());
            }
            validate_ssh_config(&String::from_utf8_lossy(&output.stdout))?;
            let reservation = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = reservation.local_addr().map_err(|e| e.to_string())?.port();
            drop(reservation);
            watch_child(ssh_command(host, Some(port)), &mut self.children, done)?;
            wait_for_socks(port, cancelled)?;
            self.socks_port = Some(port);
        }
        check_cancelled(cancelled)?;
        let empty = proxy::Environment::default();
        let environment = if embedded {
            Some(launch.proxy.as_ref().unwrap_or(&empty))
        } else {
            // Transport 19 (None) still uses Chrome's legacy policy. Transport
            // 20's explicit empty policy still disables its system proxy.
            launch.proxy.as_ref().filter(|env| !env.is_empty())
        };
        if let Some(env) = environment {
            self.forwarder = Some(proxy::Forwarder::start(env, self.socks_port)?);
        }
        check_cancelled(cancelled)
    }

    fn chrome_proxy(&self) -> Option<(&'static str, u16)> {
        self.forwarder
            .as_ref()
            .map(|f| ("http", f.port))
            .or_else(|| self.socks_port.map(|port| ("socks5", port)))
    }
}

fn wait_for_socks(port: u16, cancelled: &mpsc::Receiver<()>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        check_cancelled(cancelled)?;
        if let Ok(mut socket) =
            TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(100))
        {
            socket
                .set_read_timeout(Some(Duration::from_millis(300)))
                .map_err(|e| e.to_string())?;
            socket
                .set_write_timeout(Some(Duration::from_millis(300)))
                .map_err(|e| e.to_string())?;
            let mut reply = [0; 2];
            if socket.write_all(&[5, 1, 0]).is_ok()
                && socket.read_exact(&mut reply).is_ok()
                && reply == [5, 0]
            {
                return check_cancelled(cancelled);
            }
        }
        if Instant::now() >= deadline {
            return Err(
                "SSH SOCKS5 startup timed out; check SSH access and forwarding permission".into(),
            );
        }
        match cancelled.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => return Err("Browser connection closed before startup".into()),
        }
    }
}

fn launch(
    launch: &LaunchRequest,
    done: &mpsc::Sender<()>,
    cancelled: &mpsc::Receiver<()>,
) -> Result<Resources, String> {
    validate_launch(launch)?;
    let browser = find_chrome()?;
    let profile = new_profile()?;
    let mut resources = Resources {
        children: Children::default(),
        profile,
        connection: Connection::default(),
    };
    resources
        .connection
        .prepare(launch, done, cancelled, false)?;
    let mut chrome = Command::new(browser);
    chrome.args(chrome_args(
        &resources.profile,
        resources.connection.chrome_proxy(),
        launch.proxy.is_some(),
        &launch.request.url,
    ));
    watch_child(chrome, &mut resources.children, done)?;
    Ok(resources)
}

fn insert_ssh_option(command: Command, option: &str) -> Command {
    let mut result = Command::new(command.get_program());
    result.arg(option).args(command.get_args());
    result
}

fn ssh_command(host: &str, port: Option<u16>) -> Command {
    let mut options = vec!["-N".to_string()];
    // Never share a persistent master: the browser owns this connection and
    // dropping it must remove its listener without affecting Luvus displays.
    for option in [
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ExitOnForwardFailure=yes",
        "ClearAllForwardings=no",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "RequestTTY=no",
        "ForkAfterAuthentication=no",
    ] {
        options.extend(["-o".into(), option.into()]);
    }
    if let Some(port) = port {
        options.extend(["-D".into(), format!("127.0.0.1:{port}")]);
    }
    crate::session::remote::ssh_base_with_options(host, &options)
}

fn validate_ssh_config(config: &str) -> Result<(), String> {
    if config.len() > MAX_SSH_CONFIG_BYTES {
        return Err("SSH configuration is too large to validate browser forwarding safely".into());
    }
    for line in config.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        if matches!(key, "remoteforward" | "localforward" | "dynamicforward")
            && value.trim() != "none"
        {
            return Err(format!("Browser SSH alias contains {key}; use an alias without configured forwarding. Only Luvus's loopback -D forward is allowed"));
        }
    }
    Ok(())
}

fn chrome_args(
    profile: &Path,
    proxy: Option<(&str, u16)>,
    owner_environment: bool,
    url: &str,
) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--new-window".into(),
    ];
    if let Some((scheme, port)) = proxy {
        args.extend([
            format!("--proxy-server={scheme}://127.0.0.1:{port}"),
            "--proxy-bypass-list=<-loopback>".into(),
            "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1".into(),
            "--disable-quic".into(),
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
        ]);
    } else if owner_environment {
        args.push("--no-proxy-server".into());
    }
    args.push(url.into());
    args
}

fn find_chrome() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("LUVUS_BROWSER") {
        return Ok(path.into());
    }
    #[cfg(target_os = "macos")]
    for root in [
        PathBuf::from("/Applications"),
        crate::platform::home_dir()
            .unwrap_or_default()
            .join("Applications"),
    ] {
        for name in [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Chromium.app/Contents/MacOS/Chromium",
        ] {
            let path = root.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "chrome.exe",
        ] {
            let path = directory.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(
        "Chrome/Chromium not found on the display host; set LUVUS_BROWSER to its executable path"
            .into(),
    )
}

fn new_profile() -> Result<PathBuf, String> {
    let root = crate::persist::ensure_config_dir().join("browser-sessions");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = root.join(name);
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Rendering and hit testing share exactly one row, including narrow panes.
pub fn link_rect(area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1) / 2,
        area.width,
        u16::from(area.height > 0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded_request() -> LaunchRequest {
        LaunchRequest {
            request: Request {
                url: BLANK_URL.into(),
                proxied: true,
                hosts: Vec::new(),
                origin: None,
            },
            proxy: Some(proxy::Environment::default()),
        }
    }

    fn fixture_socket(port: u16) -> TcpStream {
        let socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
    }

    fn fixture_accept(listener: &TcpListener) -> TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    return socket;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "browser fixture did not connect");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("browser fixture accept: {error}"),
            }
        }
    }

    fn fixture_headers(socket: &mut TcpStream) -> String {
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 65536);
        }
        String::from_utf8(headers).unwrap()
    }

    #[test]
    fn browser_embedded_empty_and_legacy_policy_always_use_http_forwarder() {
        for policy in [Some(proxy::Environment::default()), None] {
            let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
            let destination = upstream.local_addr().unwrap().port();
            let mut request = embedded_request();
            request.proxy = policy;
            let (stop, cancelled) = mpsc::channel();
            let mut lease = NetworkLease::open(&request, stop, cancelled).unwrap();
            let port = lease.port();
            let closed = lease.take_closed().unwrap();
            assert!(lease.take_closed().is_none());
            let mut client = fixture_socket(port);
            write!(client, "GET http://127.0.0.1:{destination}/embedded HTTP/1.1\r\nHost: wrong\r\nProxy-Authorization: secret\r\nConnection: close\r\n\r\n").unwrap();
            let mut owner = fixture_accept(&upstream);
            let headers = fixture_headers(&mut owner).to_ascii_lowercase();
            assert!(headers.starts_with("get /embedded http/1.1"));
            assert!(!headers.contains("proxy-authorization"));
            owner
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nPASS")
                .unwrap();
            assert!(fixture_headers(&mut client).starts_with("HTTP/1.1 200"));
            let mut body = [0; 4];
            client.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"PASS");
            drop(lease);
            assert_eq!(closed.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));
            assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
        }
    }

    #[test]
    fn browser_embedded_cancellation_revokes_listener_and_open_connections() {
        let (stop, cancelled) = mpsc::channel();
        let mut lease = NetworkLease::open(&embedded_request(), stop.clone(), cancelled).unwrap();
        let closed = lease.take_closed().unwrap();
        let port = lease.port();
        let mut client = fixture_socket(port);
        // Leave a connection waiting for headers; cancellation must abort it too.
        client.write_all(b"GET ").unwrap();
        stop.send(()).unwrap();
        assert_eq!(closed.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));
        assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
        let mut byte = [0];
        match client.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("revoked browser connection remained open: {result:?}"),
        }
        drop(lease);
    }

    #[test]
    fn browser_embedded_cancelled_startup_never_runs_ssh() {
        let mut request = embedded_request();
        // This is syntactically valid but must never reach SSH discovery.
        request.request.hosts.push("browser-fixture.invalid".into());
        let (stop, cancelled) = mpsc::channel();
        stop.send(()).unwrap();
        let error = NetworkLease::open(&request, stop, cancelled).err().unwrap();
        assert_eq!(error, "Browser request cancelled");
        let (stop, cancelled) = mpsc::channel();
        drop(stop);
        assert!(check_cancelled(&cancelled).is_err());
    }

    #[test]
    fn browser_socks_readiness_honors_cancellation_during_greeting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, cancelled) = mpsc::channel();
        let worker = std::thread::spawn(move || wait_for_socks(port, &cancelled));
        let mut socket = fixture_accept(&listener);
        let mut greeting = [0; 3];
        socket.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stop.send(()).unwrap();
        socket.write_all(&[5, 0]).unwrap();
        assert!(worker.join().unwrap().is_err());
    }

    #[test]
    fn browser_embedded_rejects_bad_requests_routes_and_owner_policy() {
        let mut invalid = Vec::new();
        for url in [
            "file:///private/test",
            "chrome://settings",
            "javascript:alert(1)",
        ] {
            let mut request = embedded_request();
            request.request.url = url.into();
            invalid.push(request);
        }
        for hosts in [
            vec!["-R".into()],
            vec!["bad alias".into()],
            vec!["a".into(), "b".into()],
        ] {
            let mut request = embedded_request();
            request.request.hosts = hosts;
            invalid.push(request);
        }
        for value in [
            "http://user:secret@proxy\r\nInjected: yes",
            "https://user:secret@proxy",
        ] {
            let mut request = embedded_request();
            request.proxy.as_mut().unwrap().http = Some(value.into());
            invalid.push(request);
        }
        for request in invalid {
            let (stop, cancelled) = mpsc::channel();
            let error = NetworkLease::open(&request, stop, cancelled).err().unwrap();
            assert!(!error.contains("secret"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn browser_embedded_child_exit_revokes_network_and_reports_failure() {
        let (stop, cancelled) = mpsc::channel();
        let mut connection =
            Connection::open(&embedded_request(), &stop, &cancelled, true).unwrap();
        // Local child fixture exercises the same reaper signal as SSH, without
        // consulting user SSH configuration or contacting an owner server.
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        watch_child(command, &mut connection.children, &stop).unwrap();
        let port = connection.forwarder.as_ref().unwrap().port;
        let (closed, notification) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = closed.send(NetworkLease::wait_closed(connection, &cancelled));
        });
        assert_eq!(
            notification.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err("SSH browser connection closed".into())
        );
        worker.join().unwrap();
        assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
    }

    #[test]
    fn browser_external_preserves_legacy_empty_and_owner_proxy_modes() {
        for policy in [
            None,
            Some(proxy::Environment::default()),
            Some(proxy::Environment {
                no_proxy: Some("localhost".into()),
                ..Default::default()
            }),
        ] {
            let mut request = embedded_request();
            request.proxy = policy;
            let (done, cancelled) = mpsc::channel();
            let mut connection = Connection::open(&request, &done, &cancelled, false).unwrap();
            let has_policy = request.proxy.as_ref().is_some_and(|env| !env.is_empty());
            assert_eq!(connection.forwarder.is_some(), has_policy);
            let args = chrome_args(
                Path::new("/unused-profile"),
                connection.chrome_proxy(),
                request.proxy.is_some(),
                &request.request.url,
            );
            assert_eq!(
                args.contains(&"--no-proxy-server".into()),
                request.proxy.is_some() && !has_policy
            );
            assert_eq!(
                connection.chrome_proxy().map(|(scheme, _)| scheme),
                has_policy.then_some("http")
            );
            // A legacy or empty remote policy still goes straight to SSH SOCKS,
            // while a nonempty policy retains the Rust HTTP adapter in front.
            connection.socks_port = Some(1081);
            assert_eq!(
                connection.chrome_proxy().map(|(scheme, _)| scheme),
                Some(if has_policy { "http" } else { "socks5" })
            );
        }
    }

    #[test]
    fn blank_page_is_only_allowed_for_dedicated_browser_sessions() {
        let mut request = Request {
            url: BLANK_URL.into(),
            proxied: true,
            hosts: vec!["dev".into()],
            origin: None,
        };
        assert!(request.validate().is_ok());
        request.proxied = false;
        assert!(request.validate().is_err());
        request.proxied = true;
        for url in [
            "about:config",
            "chrome://settings",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            request.url = url.into();
            assert!(request.validate().is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn browser_watch_child_terminates_and_reaps_its_root() {
        let _env = crate::persist::test_env("browser-child-cleanup");
        let profile = new_profile().unwrap();
        let pid_file = crate::persist::config_dir().join("browser-child.pid");
        let mut resources = Resources {
            children: Children::default(),
            profile: profile.clone(),
            connection: Connection::default(),
        };
        let (done, exited) = mpsc::channel();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf '%s' \"$$\" > \"$1\"; exec sleep 30",
                "browser-child",
            ])
            .arg(&pid_file)
            .env("PATH", "/usr/bin:/bin");
        watch_child(command, &mut resources.children, &done).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "browser test child did not start"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let marker = crate::platform::process_start_marker(pid);
        drop(resources);
        let automatically_reaped = exited.recv_timeout(Duration::from_secs(1)).is_ok();
        // A failing regression must still stop its own helper and wait for the
        // watch_child reaper before panicking; never signal a reused PID.
        if !automatically_reaped {
            if marker.is_some() && crate::platform::process_start_marker(pid) == marker {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
            exited.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(
            automatically_reaped,
            "browser cleanup left its root running"
        );
        assert!(!profile.exists(), "reaped browser profile was retained");
    }

    #[test]
    fn browser_rejects_oversized_ssh_configuration() {
        let config = format!("xauthlocation {}\n", "x".repeat(64 * 1024));
        assert!(validate_ssh_config(&config).is_err());
    }

    #[test]
    fn browser_forwarding_is_local_only_and_rejects_inherited_reverse_forwarding() {
        let cmd = ssh_command("dev", Some(1081));
        let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|p| p == ["-D", "127.0.0.1:1081"]));
        assert!(!args.contains(&"-R"));
        assert_eq!(args.last(), Some(&"dev"));
        for key in ["remoteforward", "localforward", "dynamicforward"] {
            assert!(
                validate_ssh_config(&format!("hostname dev\n{key} 9000 localhost:80")).is_err()
            );
            assert!(validate_ssh_config(&format!("hostname dev\n{key} none\n")).is_ok());
        }
        let probe = insert_ssh_option(ssh_command("dev", None), "-G");
        let args: Vec<_> = probe.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args.first(), Some(&"-G"));
        assert!(!args.contains(&"-D"));
        assert!(validate_ssh_config("hostname dev\n").is_ok());
    }
    #[test]
    fn browser_chrome_localhost_uses_proxy_without_direct_fallback() {
        let args = chrome_args(
            Path::new("/profile with spaces"),
            Some(("socks5", 1081)),
            false,
            "http://localhost:3000/a?q=hello world",
        );
        assert!(args.contains(&"--proxy-bypass-list=<-loopback>".into()));
        assert!(args.contains(&"--user-data-dir=/profile with spaces".into()));
        assert_eq!(
            args.last().unwrap(),
            "http://localhost:3000/a?q=hello world"
        );
        assert!(!args
            .iter()
            .any(|a| a.contains("direct://") || a == "--no-sandbox"));
    }
}
