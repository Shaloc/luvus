//! Owner-supplied proxy policy and the display's bounded HTTP forwarding adapter.
//! Destinations (including proxy endpoints and NO_PROXY matches) connect through
//! the existing SSH SOCKS tunnel when the workspace is remote. Nothing listens
//! on the owner, and credentials never enter Chrome argv or its profile.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use hyper::body::Incoming;
use hyper::http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_VALUE: usize = 8192;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Environment {
    pub http: Option<String>,
    pub https: Option<String>,
    pub all: Option<String>,
    pub no_proxy: Option<String>,
}

impl fmt::Debug for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyEnvironment")
            .field("http", &self.http.is_some())
            .field("https", &self.https.is_some())
            .field("all", &self.all.is_some())
            .field("no_proxy", &self.no_proxy.is_some())
            .finish()
    }
}

impl Environment {
    pub fn capture() -> Self {
        Self::read(|key| {
            std::env::var_os(key).map(|v| {
                v.into_string()
                    .unwrap_or_else(|_| "\0invalid proxy environment".into())
            })
        })
    }

    fn read(mut get: impl FnMut(&str) -> Option<String>) -> Self {
        let mut value = |lower: &str, upper: &str| {
            get(lower)
                .filter(|v| !v.is_empty())
                .or_else(|| get(upper).filter(|v| !v.is_empty()))
                .map(|v| {
                    if v.len() > MAX_VALUE {
                        "\0proxy environment too large".into()
                    } else {
                        v
                    }
                })
        };
        Self {
            http: value("http_proxy", "HTTP_PROXY"),
            https: value("https_proxy", "HTTPS_PROXY"),
            all: value("all_proxy", "ALL_PROXY"),
            no_proxy: value("no_proxy", "NO_PROXY"),
        }
    }

    pub fn is_empty(&self) -> bool {
        [&self.http, &self.https, &self.all, &self.no_proxy]
            .iter()
            .all(|v| v.is_none())
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("HTTP_PROXY", &self.http),
            ("HTTPS_PROXY", &self.https),
            ("ALL_PROXY", &self.all),
            ("NO_PROXY", &self.no_proxy),
        ] {
            if value
                .as_ref()
                .is_some_and(|v| v.len() > MAX_VALUE || v.chars().any(char::is_control))
            {
                return Err(format!("Invalid or oversized owner {name}"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Scheme {
    Http,
    Socks5,
}

struct Endpoint {
    scheme: Scheme,
    host: String,
    port: u16,
    credentials: Option<(Vec<u8>, Vec<u8>)>,
}

impl Endpoint {
    fn parse(value: &str) -> Result<Self, ()> {
        let value = if value.contains("://") {
            value.to_owned()
        } else {
            format!("http://{value}")
        };
        let (scheme, rest) = value.split_once("://").ok_or(())?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "http" => Scheme::Http,
            "socks5" | "socks5h" => Scheme::Socks5,
            _ => return Err(()),
        };
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.contains(['/', '?', '#']) {
            return Err(());
        }
        let (credentials, address) = match rest.rsplit_once('@') {
            Some((auth, address)) => {
                let (user, password) = auth.split_once(':').unwrap_or((auth, ""));
                let user = percent_decode(user)?;
                let password = percent_decode(password)?;
                if user.is_empty()
                    || user.contains(&b':')
                    || user.len() > 255
                    || password.len() > 255
                {
                    return Err(());
                }
                (Some((user, password)), address)
            }
            None => (None, rest),
        };
        let authority = address
            .parse::<hyper::http::uri::Authority>()
            .map_err(|_| ())?;
        if authority.host().is_empty() || authority.host().contains('@') || invalid_port(&authority)
        {
            return Err(());
        }
        let port = authority
            .port_u16()
            .unwrap_or(if scheme == Scheme::Http { 80 } else { 1080 });
        if port == 0 {
            return Err(());
        }
        Ok(Self {
            scheme,
            host: unbracket(authority.host()).to_owned(),
            port,
            credentials,
        })
    }

    fn authorization(&self) -> Option<HeaderValue> {
        let (user, password) = self.credentials.as_ref()?;
        let mut bytes = user.clone();
        bytes.push(b':');
        bytes.extend(password);
        HeaderValue::from_str(&format!("Basic {}", crate::base64_encode(&bytes))).ok()
    }
}

fn percent_decode(value: &str) -> Result<Vec<u8>, ()> {
    let mut bytes = value.as_bytes().iter().copied();
    let mut output = Vec::new();
    while let Some(byte) = bytes.next() {
        output.push(if byte == b'%' {
            let a = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
            let b = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
            (a * 16 + b) as u8
        } else {
            byte
        });
    }
    Ok(output)
}

fn invalid_port(authority: &hyper::http::uri::Authority) -> bool {
    let suffix = &authority.as_str()[authority.host().len()..];
    !suffix.is_empty()
        && (suffix
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .is_none())
}

fn unbracket(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(host)
}

struct Policy {
    http: Option<Endpoint>,
    https: Option<Endpoint>,
    all: Option<Endpoint>,
    bypass: Vec<String>,
    ssh_port: Option<u16>,
}

impl Policy {
    fn new(env: &Environment, ssh_port: Option<u16>) -> Result<Self, String> {
        env.validate()?;
        let endpoint = |name, value: &Option<String>| {
            value
                .as_ref()
                .map(|v| {
                    Endpoint::parse(v).map_err(|_| {
                        format!(
                            "Invalid owner {name}; use an http:// or socks5[h]:// proxy endpoint"
                        )
                    })
                })
                .transpose()
        };
        let bypass: Vec<_> = env
            .no_proxy
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if bypass.len() > 256 {
            return Err("Too many owner NO_PROXY entries (limit 256)".into());
        }
        Ok(Self {
            http: endpoint("HTTP_PROXY", &env.http)?,
            https: endpoint("HTTPS_PROXY", &env.https)?,
            all: endpoint("ALL_PROXY", &env.all)?,
            bypass,
            ssh_port,
        })
    }

    fn endpoint(&self, host: &str, port: u16, secure: bool) -> Option<&Endpoint> {
        if self
            .bypass
            .iter()
            .any(|rule| bypass_matches(rule, host, port))
        {
            return None;
        }
        (if secure {
            self.https.as_ref()
        } else {
            self.http.as_ref()
        })
        .or(self.all.as_ref())
    }

    async fn owner_connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        if let Some(ssh_port) = self.ssh_port {
            let mut stream = TcpStream::connect(("127.0.0.1", ssh_port))
                .await
                .map_err(|_| "SSH proxy unavailable")?;
            socks_connect(&mut stream, host, port, None).await?;
            Ok(stream)
        } else {
            TcpStream::connect((host, port))
                .await
                .map_err(|_| "Owner destination unavailable".into())
        }
    }
}

fn bypass_matches(rule: &str, host: &str, port: u16) -> bool {
    if rule == "*" {
        return true;
    }
    let host = unbracket(host).trim_end_matches('.').to_ascii_lowercase();
    if let Some((network, prefix)) = rule.split_once('/') {
        return match (
            unbracket(network).parse::<IpAddr>(),
            host.parse::<IpAddr>(),
            prefix.parse::<u32>(),
        ) {
            (Ok(IpAddr::V4(a)), Ok(IpAddr::V4(b)), Ok(n)) if n <= 32 => {
                n == 0 || u32::from(a) >> (32 - n) == u32::from(b) >> (32 - n)
            }
            (Ok(IpAddr::V6(a)), Ok(IpAddr::V6(b)), Ok(n)) if n <= 128 => {
                n == 0 || u128::from(a) >> (128 - n) == u128::from(b) >> (128 - n)
            }
            _ => false,
        };
    }
    let (name, rule_port) = if rule.starts_with('[') {
        let Some(end) = rule.find(']') else {
            return false;
        };
        let suffix = &rule[end + 1..];
        (
            &rule[1..end],
            if suffix.is_empty() {
                None
            } else {
                Some(suffix.strip_prefix(':').unwrap_or(""))
            },
        )
    } else if rule.matches(':').count() == 1 {
        let (name, port) = rule.rsplit_once(':').unwrap();
        (name, Some(port))
    } else {
        (rule, None)
    };
    if rule_port.is_some_and(|p| p.parse::<u16>().ok() != Some(port)) {
        return false;
    }
    let name = name
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if let Ok(ip) = name.parse::<IpAddr>() {
        return host.parse::<IpAddr>().ok() == Some(ip);
    }
    host == name
        || host
            .strip_suffix(&name)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

async fn socks_connect(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    credentials: Option<&(Vec<u8>, Vec<u8>)>,
) -> Result<(), String> {
    stream
        .write_all(if credentials.is_some() {
            &[5, 2, 0, 2]
        } else {
            &[5, 1, 0]
        })
        .await
        .map_err(|_| "SOCKS greeting failed")?;
    let mut reply = [0; 2];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|_| "SOCKS greeting failed")?;
    if reply == [5, 2] {
        let (user, password) = credentials.ok_or("SOCKS authentication required")?;
        let mut auth = vec![1, user.len() as u8];
        auth.extend(user);
        auth.push(password.len() as u8);
        auth.extend(password);
        stream
            .write_all(&auth)
            .await
            .map_err(|_| "SOCKS authentication failed")?;
        stream
            .read_exact(&mut reply)
            .await
            .map_err(|_| "SOCKS authentication failed")?;
        if reply != [1, 0] {
            return Err("SOCKS authentication rejected".into());
        }
    } else if reply != [5, 0] {
        return Err("SOCKS authentication unsupported".into());
    }
    let mut request = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend(ip.octets());
        }
        Err(_) => {
            if host.is_empty() || host.len() > 255 {
                return Err("Invalid SOCKS destination".into());
            }
            request.extend([3, host.len() as u8]);
            request.extend(host.as_bytes());
        }
    }
    request.extend(port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|_| "SOCKS connect failed")?;
    let mut reply = [0; 4];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|_| "SOCKS connect failed")?;
    if reply[..3] != [5, 0, 0] {
        return Err("SOCKS destination rejected".into());
    }
    let size = match reply[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await.map_err(|_| "SOCKS reply failed")? as usize,
        _ => return Err("Invalid SOCKS reply".into()),
    };
    let mut address = vec![0; size + 2];
    stream
        .read_exact(&mut address)
        .await
        .map_err(|_| "SOCKS reply failed")?;
    Ok(())
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Tunnel = Box<dyn Io>;

async fn exchange(stream: TcpStream, request: Request<Body>) -> Result<Response<Incoming>, String> {
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| "Proxy HTTP handshake failed")?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    sender
        .send_request(request)
        .await
        .map_err(|_| "Proxy HTTP request failed".into())
}

async fn upstream(
    policy: &Policy,
    host: &str,
    port: u16,
    endpoint: Option<&Endpoint>,
) -> Result<TcpStream, String> {
    let Some(endpoint) = endpoint else {
        return policy.owner_connect(host, port).await;
    };
    let mut stream = policy.owner_connect(&endpoint.host, endpoint.port).await?;
    if endpoint.scheme == Scheme::Socks5 {
        socks_connect(&mut stream, host, port, endpoint.credentials.as_ref()).await?;
    }
    Ok(stream)
}

async fn connect_tunnel(
    policy: &Policy,
    host: &str,
    port: u16,
    endpoint: Option<&Endpoint>,
) -> Result<Tunnel, String> {
    let stream = upstream(policy, host, port, endpoint).await?;
    if let Some(endpoint) = endpoint.filter(|e| e.scheme == Scheme::Http) {
        let address = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let mut request = Request::builder()
            .method(Method::CONNECT)
            .uri(&address)
            .header(header::HOST, &address)
            .body(Body::empty())
            .map_err(|_| "Invalid CONNECT destination")?;
        if let Some(auth) = endpoint.authorization() {
            request
                .headers_mut()
                .insert(header::PROXY_AUTHORIZATION, auth);
        }
        let mut response = exchange(stream, request).await?;
        if !response.status().is_success() {
            return Err("Owner HTTP proxy rejected CONNECT".into());
        }
        let upgraded = hyper::upgrade::on(&mut response)
            .await
            .map_err(|_| "Owner proxy tunnel failed")?;
        Ok(Box::new(TokioIo::new(upgraded)))
    } else {
        Ok(Box::new(stream))
    }
}

fn clean_headers(headers: &mut HeaderMap, websocket: bool) {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_owned())
        .collect();
    for name in nominated {
        if !websocket || !name.eq_ignore_ascii_case("upgrade") {
            headers.remove(name);
        }
    }
    for name in [
        "connection",
        "proxy-connection",
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    } else {
        headers.remove(header::UPGRADE);
    }
}

async fn forward(
    mut request: Request<Incoming>,
    policy: Arc<Policy>,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Response<Body>, String> {
    let connect = request.method() == Method::CONNECT;
    let authority = request
        .uri()
        .authority()
        .ok_or("Expected an absolute browser URL")?
        .clone();
    if authority.as_str().contains('@') || invalid_port(&authority) {
        return Err("Invalid browser destination".into());
    }
    if !connect && request.uri().scheme_str() != Some("http") {
        return Err("Expected HTTP or CONNECT".into());
    }
    let host = unbracket(authority.host());
    let port = authority
        .port_u16()
        .unwrap_or(if connect { 443 } else { 80 });
    if port == 0 {
        return Err("Invalid browser destination port".into());
    }
    let endpoint = policy.endpoint(host, port, connect);
    if connect {
        let mut tunnel = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connect_tunnel(&policy, host, port, endpoint),
        )
        .await
        .map_err(|_| "Owner proxy connection timed out")??;
        let incoming = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let _permit = permit;
            if let Ok(upgraded) = incoming.await {
                let _ =
                    tokio::io::copy_bidirectional(&mut TokioIo::new(upgraded), &mut tunnel).await;
            }
        });
        return Ok(Response::new(Body::empty()));
    }
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, upstream(&policy, host, port, endpoint))
        .await
        .map_err(|_| "Owner proxy connection timed out")??;
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let incoming_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    clean_headers(request.headers_mut(), websocket);
    request.headers_mut().insert(
        header::HOST,
        HeaderValue::from_str(authority.as_str()).map_err(|_| "Invalid browser host")?,
    );
    if let Some(endpoint) = endpoint.filter(|e| e.scheme == Scheme::Http) {
        if let Some(auth) = endpoint.authorization() {
            request
                .headers_mut()
                .insert(header::PROXY_AUTHORIZATION, auth);
        }
    } else {
        *request.uri_mut() = request
            .uri()
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
            .parse::<Uri>()
            .map_err(|_| "Invalid browser path")?;
    }
    let mut response = exchange(stream, request.map(Body::new)).await?;
    if response.status() == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
        return Err("Owner proxy authentication rejected".into());
    }
    let upgrade = websocket && response.status() == StatusCode::SWITCHING_PROTOCOLS;
    if let Some(incoming) = incoming_upgrade.filter(|_| upgrade) {
        let outgoing = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            let _permit = permit;
            if let (Ok(a), Ok(b)) = tokio::join!(incoming, outgoing) {
                let _ =
                    tokio::io::copy_bidirectional(&mut TokioIo::new(a), &mut TokioIo::new(b)).await;
            }
        });
    }
    clean_headers(response.headers_mut(), upgrade);
    Ok(response.map(Body::new))
}

pub struct Forwarder {
    pub port: u16,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Forwarder {
    pub fn start(env: &Environment, ssh_port: Option<u16>) -> Result<Self, String> {
        let policy = Arc::new(Policy::new(env, ssh_port)?);
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| "Cannot bind browser proxy")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Cannot read browser proxy address")?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|_| "Cannot initialize browser proxy")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "Cannot start browser proxy runtime")?;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return; };
                let slots = Arc::new(tokio::sync::Semaphore::new(64));
                let mut tasks = tokio::task::JoinSet::new();
                tokio::pin!(stopped);
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        _ = tasks.join_next(), if !tasks.is_empty() => {},
                        result = listener.accept() => {
                            let Ok((stream, peer)) = result else { break; };
                            let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                            if !peer.ip().is_loopback() { continue; }
                            let policy = policy.clone();
                            let permit = Arc::new(permit);
                            tasks.spawn(async move {
                                let service = hyper::service::service_fn(move |request| {
                                    let policy = policy.clone(); let permit = permit.clone();
                                    async move {
                                        let response = forward(request, policy, permit).await.unwrap_or_else(|_| {
                                            let mut response = Response::new(Body::from("Owner browser proxy connection failed"));
                                            *response.status_mut() = StatusCode::BAD_GATEWAY;
                                            response
                                        });
                                        Ok::<_, std::convert::Infallible>(response)
                                    }
                                });
                                let _ = hyper::server::conn::http1::Builder::new()
                                    .timer(hyper_util::rt::TokioTimer::new())
                                    .header_read_timeout(CONNECT_TIMEOUT).max_buf_size(64 * 1024).max_headers(128)
                                    .serve_connection(TokioIo::new(stream), service).with_upgrades().await;
                            });
                        }
                    }
                }
                tasks.abort_all();
            });
            // Stop tunnels, client drivers and any pending DNS work on detach.
            runtime.shutdown_timeout(Duration::from_secs(1));
        });
        Ok(Self {
            port,
            stop: Some(stop),
            worker: Some(worker),
        })
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(port: u16) -> std::net::TcpStream {
        let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    }

    fn headers(s: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut result = Vec::new();
        while !result.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            s.read_exact(&mut byte).unwrap();
            result.push(byte[0]);
            assert!(result.len() < 65536);
        }
        String::from_utf8(result).unwrap()
    }

    #[test]
    fn proxy_streams_post_body_and_websocket_then_closes_listener() {
        use std::io::{Read, Write};
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = upstream.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut s, _) = upstream.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let h = headers(&mut s).to_lowercase();
            assert!(h.starts_with("post /upload?q=1 http/1.1"));
            assert!(!h.contains("proxy-authorization"));
            let mut body = [0; 4];
            s.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"DATA");
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nPASS")
                .unwrap();
            let (mut s, _) = upstream.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let h = headers(&mut s).to_lowercase();
            assert!(h.contains("upgrade: websocket"));
            s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").unwrap();
            s.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"PING");
            s.write_all(b"PONG").unwrap();
        });
        let forwarder = Forwarder::start(&Environment::default(), None).unwrap();
        let mut s = socket(forwarder.port);
        write!(s, "POST http://127.0.0.1:{port}/upload?q=1 HTTP/1.1\r\nHost: wrong\r\nContent-Length: 4\r\nProxy-Authorization: secret\r\nConnection: close\r\n\r\nDATA").unwrap();
        assert!(headers(&mut s).starts_with("HTTP/1.1 200"));
        let mut body = [0; 4];
        s.read_exact(&mut body).unwrap();
        assert_eq!(&body, b"PASS");
        let mut s = socket(forwarder.port);
        write!(s, "GET http://127.0.0.1:{port}/ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").unwrap();
        assert!(headers(&mut s).starts_with("HTTP/1.1 101"));
        s.write_all(b"PING").unwrap();
        s.read_exact(&mut body).unwrap();
        assert_eq!(&body, b"PONG");
        worker.join().unwrap();
        let port = forwarder.port;
        drop(forwarder);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn proxy_all_socks_authentication_and_remote_dns() {
        use std::io::{Read, Write};
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = upstream.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut s, _) = upstream.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut greeting = [0; 4];
            s.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [5, 2, 0, 2]);
            s.write_all(&[5, 2]).unwrap();
            let mut auth = [0; 11];
            s.read_exact(&mut auth).unwrap();
            assert_eq!(&auth, b"\x01\x04user\x04pass");
            s.write_all(&[1, 0]).unwrap();
            let mut dest = [0; 5];
            s.read_exact(&mut dest).unwrap();
            assert_eq!(&dest[..4], &[5, 1, 0, 3]);
            let mut name = vec![0; dest[4] as usize + 2];
            s.read_exact(&mut name).unwrap();
            assert_eq!(&name, b"only-at-owner.invalid\x01\xbb");
            s.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
            let mut data = [0; 4];
            s.read_exact(&mut data).unwrap();
            assert_eq!(&data, b"PING");
            s.write_all(b"PONG").unwrap();
        });
        let forwarder = Forwarder::start(
            &Environment {
                all: Some(format!("socks5h://user:pass@127.0.0.1:{port}")),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let mut s = socket(forwarder.port);
        s.write_all(b"CONNECT only-at-owner.invalid:443 HTTP/1.1\r\nHost: only-at-owner.invalid:443\r\n\r\n").unwrap();
        assert!(headers(&mut s).starts_with("HTTP/1.1 200"));
        s.write_all(b"PING").unwrap();
        let mut data = [0; 4];
        s.read_exact(&mut data).unwrap();
        assert_eq!(&data, b"PONG");
        worker.join().unwrap();
    }

    #[test]
    fn proxy_environment_precedence_and_credentials_redaction() {
        let env = Environment::read(|k| match k {
            "http_proxy" => Some("http://user:secret@lower:8080".into()),
            "HTTP_PROXY" => Some("http://upper:8080".into()),
            "HTTPS_PROXY" => Some("http://secure:8080".into()),
            "ALL_PROXY" => Some("socks5h://fallback:1080".into()),
            "NO_PROXY" => Some("localhost,.internal.test,10.0.0.0/8,[::1]:3000".into()),
            _ => None,
        });
        assert!(!format!("{env:?}").contains("secret"));
        let policy = Policy::new(&env, Some(1234)).unwrap();
        assert_eq!(
            policy.endpoint("outside.test", 80, false).unwrap().host,
            "lower"
        );
        assert_eq!(
            policy.endpoint("outside.test", 443, true).unwrap().host,
            "secure"
        );
        for (host, port) in [
            ("localhost", 80),
            ("a.internal.test", 80),
            ("10.9.8.7", 80),
            ("::1", 3000),
        ] {
            assert!(policy.endpoint(host, port, false).is_none(), "{host}");
        }
        assert!(policy.endpoint("notinternal.test", 80, false).is_some());
        assert!(policy.endpoint("::1", 3001, false).is_some());
        let fallback = Policy::new(
            &Environment {
                all: env.all,
                ..Environment::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            fallback.endpoint("outside.test", 8443, true).unwrap().host,
            "fallback"
        );
        assert!(bypass_matches("*", "any.test", 123));
        assert!(bypass_matches("2001:db8::/32", "2001:db8::1", 80));
        assert!(!bypass_matches("10.0.0.0/8", "110.1.2.3", 80));
    }

    #[test]
    fn proxy_endpoint_validation_never_exposes_secrets() {
        for value in [
            "https://user:secret@proxy",
            "file:///secret",
            "http://user:secret@proxy:99999",
            "http://proxy/path",
        ] {
            let error = Policy::new(
                &Environment {
                    http: Some(value.into()),
                    ..Environment::default()
                },
                None,
            )
            .err()
            .unwrap();
            assert!(!error.contains("secret"));
        }
        let endpoint = Endpoint::parse("http://name:p%40ss@proxy:8080")
            .ok()
            .unwrap();
        assert_eq!(endpoint.authorization().unwrap(), "Basic bmFtZTpwQHNz");
        assert!(Policy::new(
            &Environment {
                http: Some("http://proxy\r\nInjected: yes".into()),
                ..Environment::default()
            },
            None
        )
        .is_err());
    }
}
