//! Display-owned Chrome sessions. Only local SSH dynamic forwarding is used;
//! the owner server never runs a browser or opens a proxy listening socket.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

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

struct Resources {
    children: Vec<(crate::platform::ChildTreeGuard, mpsc::Receiver<()>)>,
    profile: PathBuf,
    forwarder: Option<proxy::Forwarder>,
}

impl Drop for Resources {
    fn drop(&mut self) {
        drop(self.forwarder.take());
        for (child, _) in &mut self.children {
            child.refresh_descendants();
            child.terminate();
        }
        let reaped = self
            .children
            .iter()
            .all(|(_, done)| done.recv_timeout(Duration::from_secs(2)).is_ok());
        if reaped {
            let _ = std::fs::remove_dir_all(&self.profile);
        }
    }
}

fn watch_child(
    mut command: Command,
    resources: &mut Resources,
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
    resources.children.push((guard, wait));
    let done = done.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = reaped.send(());
        let _ = done.send(());
    });
    Ok(())
}

fn launch(
    launch: &LaunchRequest,
    done: &mpsc::Sender<()>,
    cancelled: &mpsc::Receiver<()>,
) -> Result<Resources, String> {
    let request = &launch.request;
    request.validate()?;
    if let Some(env) = &launch.proxy {
        env.validate()?;
    }
    let browser = find_chrome()?;
    let profile = new_profile()?;
    let mut resources = Resources {
        children: Vec::new(),
        profile,
        forwarder: None,
    };
    let proxy = if let Some(host) = request.hosts.first() {
        // Check the same effective options before opening the connection. A
        // configured RemoteForward is forbidden, including inherited defaults.
        let mut probe = ssh_command(host, None);
        // ssh accepts -G before the destination; rebuild with that option.
        probe = insert_ssh_option(probe, "-G");
        let output = crate::session::remote::run_ssh_command_with_timeout(
            probe,
            host,
            MAX_SSH_CONFIG_BYTES + 1,
            Duration::from_secs(10),
        )?;
        if !output.status.success() {
            return Err("Could not resolve the display-side SSH configuration".into());
        }
        validate_ssh_config(&String::from_utf8_lossy(&output.stdout))?;
        let reservation = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = reservation.local_addr().map_err(|e| e.to_string())?.port();
        drop(reservation);
        watch_child(ssh_command(host, Some(port)), &mut resources, done)?;
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if cancelled.try_recv().is_ok() {
                return Err("Browser connection closed before startup".into());
            }
            if let Ok(mut socket) = TcpStream::connect_timeout(
                &([127, 0, 0, 1], port).into(),
                Duration::from_millis(100),
            ) {
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
                    break;
                }
            }
            if Instant::now() >= deadline {
                return Err(
                    "SSH SOCKS5 startup timed out; check SSH access and forwarding permission"
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(port)
    } else {
        None
    };
    if cancelled.try_recv().is_ok() {
        return Err("Browser request cancelled".into());
    }
    if let Some(env) = launch.proxy.as_ref().filter(|env| !env.is_empty()) {
        resources.forwarder = Some(proxy::Forwarder::start(env, proxy)?);
    }
    let chrome_proxy = resources
        .forwarder
        .as_ref()
        .map(|f| ("http", f.port))
        .or_else(|| proxy.map(|port| ("socks5", port)));
    let mut chrome = Command::new(browser);
    chrome.args(chrome_args(
        &resources.profile,
        chrome_proxy,
        launch.proxy.is_some(),
        &request.url,
    ));
    watch_child(chrome, &mut resources, done)?;
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
            children: Vec::new(),
            profile: profile.clone(),
            forwarder: None,
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
        watch_child(command, &mut resources, &done).unwrap();
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
        assert!(validate_ssh_config("hostname dev\nremoteforward 9000 localhost:80").is_err());
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
