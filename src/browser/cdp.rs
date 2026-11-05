//! Foreground, single-client CDP transport for an existing display-owned browser.
//! This module never starts a core, a browser, or an SSH connection.

use std::fmt;
#[cfg(unix)]
mod downloads;

const USAGE: &str =
    "Usage: luvus browser cdp --browser <opaque32hex> [--ttl <1-3600>] [--json]\n\n\
Expose one temporary authenticated loopback WebSocket for an existing browser.\n\
Run this foreground command on the browser's owner; --host is not supported.\n\
The default lifetime is 300 seconds. --help shows this help.\n";

#[derive(Clone, Copy, Debug)]
struct Failure {
    code: &'static str,
    message: &'static str,
}

impl Failure {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never interpolate upstream error text, socket paths, leases, or URLs.
        write!(
            f,
            "{}",
            serde_json::json!({"error":{"code":self.code,"message":self.message}})
        )
    }
}

impl std::error::Error for Failure {}

type GatewayResult<T> = Result<T, Failure>;

struct Options {
    browser: String,
    ttl: u64,
}

fn opaque_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Options {
    fn parse(args: &[String]) -> GatewayResult<Option<Self>> {
        let invalid = || Failure::new("invalid_params", "invalid or duplicate CDP gateway option");
        let mut browser = None;
        let mut ttl = None;
        let mut json = false;
        let mut help = false;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--browser" if browser.is_none() => {
                    let value = args.next().ok_or_else(invalid)?;
                    if !opaque_id(value) {
                        return Err(Failure::new(
                            "invalid_params",
                            "--browser requires a lowercase 32-hex browser identifier",
                        ));
                    }
                    browser = Some(value.clone());
                }
                "--ttl" if ttl.is_none() => {
                    let value = args.next().ok_or_else(invalid)?;
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(invalid());
                    }
                    ttl = Some(
                        value
                            .parse::<u64>()
                            .ok()
                            .filter(|value| (1..=3600).contains(value))
                            .ok_or_else(invalid)?,
                    );
                }
                "--json" if !json => json = true,
                "--help" if !help => help = true,
                "--host" => return Err(remote_error()),
                _ => return Err(invalid()),
            }
        }
        if help {
            return Ok(None);
        }
        Ok(Some(Self {
            browser: browser.ok_or_else(|| {
                Failure::new(
                    "invalid_params",
                    "--browser is required for the CDP gateway",
                )
            })?,
            ttl: ttl.unwrap_or(300),
        }))
    }
}

fn remote_error() -> Failure {
    Failure::new(
        "unsupported_target",
        "--host is not supported; run this foreground command on the browser's owner",
    )
}

pub(crate) fn run_cli(args: &[String]) -> anyhow::Result<i32> {
    if crate::session::remote::process_target().is_some() {
        return Err(remote_error().into());
    }
    let Some(options) = Options::parse(args)? else {
        use std::io::Write;
        std::io::stdout()
            .lock()
            .write_all(USAGE.as_bytes())
            .map_err(|_| Failure::new("output_failed", "cannot write CDP gateway help"))?;
        return Ok(0);
    };
    #[cfg(unix)]
    {
        gateway::run(options)?;
        Ok(0)
    }
    #[cfg(not(unix))]
    {
        let _ = (options.browser, options.ttl);
        // Conn exposes AsRawFd on Unix, but no cancellable handle on Windows.
        // Never leave a blocking subscription reader holding a core connection.
        Err(Failure::new(
            "unsupported_platform",
            "CDP gateway requires a cancellable local API stream on this platform",
        )
        .into())
    }
}

#[cfg(unix)]
mod gateway {
    use std::collections::{HashMap, HashSet};
    use std::io::{BufReader, Write};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
    use axum::extract::{FromRequestParts, Request, State};
    use axum::http::{header, HeaderMap, Method, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::Router;
    use futures_util::stream::{SplitSink, SplitStream};
    use futures_util::{SinkExt, StreamExt};
    use serde::Deserialize;
    use serde_json::{json, Value};
    use tokio::sync::{mpsc, watch};
    use tokio::task::JoinSet;

    use super::{opaque_id, Failure, GatewayResult, Options};
    use crate::ipc::api;
    use crate::ipc::transport::{self, Conn, TimeoutMode};

    const MAX_MESSAGE: usize = 64 * 1024 * 1024;
    const MAX_API_FRAME: usize = 1024 * 1024;
    const INPUT_CHUNK: usize = 8192;
    const OUTPUT_CHUNK: usize = 128 * 1024;
    const MAX_CHUNKS: usize = 16384;
    const MAX_TRANSFERS: usize = 2;
    const OUTPUT_QUEUE: usize = 8;
    // Never evict completed IDs: doing so would permit replaying a transfer.
    const MAX_SEEN_TRANSFERS: usize = 65536;
    const RPC_TIMEOUT: Duration = Duration::from_secs(25);
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
    const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
    const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

    const CANCELLED: Failure = Failure::new("cancelled", "CDP gateway stopped");
    const BAD_RESPONSE: Failure =
        Failure::new("invalid_response", "invalid local browser API response");
    const BAD_CHUNK: Failure = Failure::new("invalid_stream", "invalid CDP stream chunk");
    const OVERFLOW: Failure = Failure::new("limit_exceeded", "CDP gateway buffer limit exceeded");

    #[derive(Clone)]
    struct Stop(watch::Sender<Option<GatewayResult<()>>>);

    impl Stop {
        fn new() -> Self {
            Self(watch::channel(None).0)
        }

        fn finish(&self, result: GatewayResult<()>) {
            self.0.send_if_modified(|current| {
                if current.is_some() {
                    return false;
                }
                *current = Some(result);
                true
            });
        }

        fn stopped(&self) -> bool {
            self.0.borrow().is_some()
        }

        async fn wait(&self) -> GatewayResult<()> {
            let mut receiver = self.0.subscribe();
            loop {
                if let Some(result) = *receiver.borrow_and_update() {
                    return result;
                }
                if receiver.changed().await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    struct Lease {
        secret: String,
    }

    #[derive(Default)]
    struct Connections {
        closed: bool,
        next: u64,
        live: HashMap<u64, Conn>,
    }

    struct Owner {
        // Captured once, including an inherited selected-session socket selector.
        socket: PathBuf,
        browser: String,
        files: OnceLock<crate::browser::files::WorkspaceFiles>,
        downloads: super::downloads::Policy,
        stop: Stop,
        lease: Mutex<Option<Lease>>,
        connections: Mutex<Connections>,
        reader: Mutex<Option<JoinHandle<()>>>,
    }

    impl Owner {
        fn params(&self) -> GatewayResult<Value> {
            let lease = self.lease.lock().unwrap_or_else(|error| error.into_inner());
            let lease = lease.as_ref().ok_or(CANCELLED)?;
            Ok(json!({"browser":self.browser,"lease":lease.secret}))
        }

        fn track(self: &Arc<Self>, connection: &Conn) -> GatewayResult<ConnectionGuard> {
            let mut connections = self
                .connections
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if connections.closed || self.stop.stopped() {
                shutdown(connection);
                return Err(CANCELLED);
            }
            // Two transfer RPCs plus the subscription; startup is sequential.
            if connections.live.len() > MAX_TRANSFERS {
                shutdown(connection);
                return Err(OVERFLOW);
            }
            connections.next += 1;
            let id = connections.next;
            connections.live.insert(id, connection.clone());
            Ok(ConnectionGuard {
                owner: Arc::clone(self),
                id,
            })
        }

        fn cancel_connections(&self) {
            let mut connections = self
                .connections
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            connections.closed = true;
            for connection in connections.live.values() {
                shutdown(connection);
            }
        }
    }

    struct ConnectionGuard {
        owner: Arc<Owner>,
        id: u64,
    }

    impl Drop for ConnectionGuard {
        fn drop(&mut self) {
            if let Some(connection) = self
                .owner
                .connections
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .live
                .remove(&self.id)
            {
                shutdown(&connection);
            }
        }
    }

    fn shutdown(connection: &Conn) {
        // Conn clones own the same descriptor. shutdown wakes the blocking reader
        // without closing/reusing the descriptor while another thread owns it.
        let _ = connection.shutdown();
    }

    struct Cleanup(Arc<Owner>);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.stop.finish(Ok(()));
            self.0.cancel_connections();
            if let Some(reader) = self
                .0
                .reader
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                let _ = reader.join();
            }
            let lease = self
                .0
                .lease
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(lease) = lease {
                // The subscription close may already have revoked this lease.
                let _ = local_request(
                    &self.0.socket,
                    "browser.control.release",
                    json!({"browser":self.0.browser,"lease":lease.secret}),
                    None,
                    RELEASE_TIMEOUT,
                );
            }
        }
    }

    pub(super) fn run(options: Options) -> GatewayResult<()> {
        let owner = Arc::new(Owner {
            socket: crate::persist::cli_socket_path(),
            browser: options.browser,
            files: OnceLock::new(),
            downloads: Default::default(),
            stop: Stop::new(),
            lease: Mutex::new(None),
            connections: Mutex::new(Connections::default()),
            reader: Mutex::new(None),
        });
        let cleanup = Cleanup(Arc::clone(&owner));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("luvus-cdp")
            .max_blocking_threads(MAX_TRANSFERS + 1)
            .build()
            .map_err(|_| Failure::new("unavailable", "cannot initialize CDP gateway runtime"))?;
        let result = runtime.block_on(run_async(Arc::clone(&owner), options.ttl));
        owner.stop.finish(result);
        owner.cancel_connections();
        // Dropping JoinSet futures cannot cancel spawn_blocking. Interrupt their
        // tracked sockets first, then join all runtime workers before releasing.
        // An in-flight acquire is deliberately allowed to finish (<=25 seconds),
        // so even a signal during acquisition cannot discard a returned lease.
        drop(runtime);
        drop(cleanup);
        result
    }

    async fn run_async(owner: Arc<Owner>, ttl: u64) -> GatewayResult<()> {
        use tokio::signal::unix::{signal, SignalKind};

        // Register all handlers before issuing any lease RPC. No detached signal
        // task or polling thread survives the foreground runtime.
        let signal_error = |_| Failure::new("unavailable", "cannot install CDP shutdown handlers");
        let mut interrupt = signal(SignalKind::interrupt()).map_err(signal_error)?;
        let mut terminate = signal(SignalKind::terminate()).map_err(signal_error)?;
        let mut hangup = signal(SignalKind::hangup()).map_err(signal_error)?;
        let expires_at = unix_seconds()?.saturating_add(ttl);
        tokio::select! {
            biased;
            _ = interrupt.recv() => Ok(()),
            _ = terminate.recv() => Ok(()),
            _ = hangup.recv() => Ok(()),
            result = owner.stop.wait() => result,
            _ = tokio::time::sleep(Duration::from_secs(ttl)) => Ok(()),
            result = serve(Arc::clone(&owner), ttl, expires_at) => result,
        }
    }

    fn unix_seconds() -> GatewayResult<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|_| Failure::new("unavailable", "system clock is before the Unix epoch"))
    }

    fn random_id() -> GatewayResult<String> {
        crate::terminal::backend::random_id()
            .map_err(|_| Failure::new("unavailable", "cannot generate CDP gateway identifier"))
    }

    fn remaining(deadline: Instant) -> GatewayResult<Duration> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| Failure::new("timeout", "local browser API deadline expired"))
    }

    fn connect(path: &Path, deadline: Instant) -> GatewayResult<Conn> {
        transport::connect_timeout(path, remaining(deadline)?.min(CONNECT_TIMEOUT))
            .map_err(|_| Failure::new("unavailable", "cannot connect to selected existing core"))
    }

    fn write_request(
        connection: &mut Conn,
        id: &str,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> GatewayResult<()> {
        let mut frame = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))
            .map_err(|_| Failure::new("invalid_request", "cannot encode local browser request"))?;
        frame.push(b'\n');
        if frame.len() > MAX_API_FRAME {
            return Err(OVERFLOW);
        }
        let deadline = deadline.min(Instant::now() + WRITE_TIMEOUT);
        let mut bytes = frame.as_slice();
        while !bytes.is_empty() {
            let mode = connection
                .set_send_timeout(remaining(deadline)?)
                .map_err(|_| Failure::new("unavailable", "cannot bound local API write"))?;
            if mode != TimeoutMode::Kernel {
                return Err(Failure::new(
                    "unavailable",
                    "local API write deadline unsupported",
                ));
            }
            match connection.write(bytes) {
                Ok(0) => return Err(Failure::new("disconnected", "local browser API closed")),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    return Err(Failure::new(
                        "unavailable",
                        "local browser API write failed",
                    ))
                }
            }
        }
        // Conn writes directly to the Unix socket; there is no userspace buffer.
        Ok(())
    }

    fn response(frame: &str, id: &str) -> GatewayResult<Value> {
        if frame.len() >= MAX_API_FRAME {
            return Err(BAD_RESPONSE);
        }
        let mut value: Value = serde_json::from_str(frame).map_err(|_| BAD_RESPONSE)?;
        if value.get("id").and_then(Value::as_str) != Some(id) {
            return Err(BAD_RESPONSE);
        }
        if let Some(error) = value.get("error") {
            // Codes are selected from our vocabulary, never echoed verbatim.
            let code = match error.get("code").and_then(Value::as_str) {
                Some("control_busy") => "control_busy",
                Some("control_required") => "control_required",
                Some("control_revoked") => "control_revoked",
                Some("control_released") => "control_released",
                Some("control_denied") => "control_denied",
                Some("browser_not_found") => "browser_not_found",
                Some("browser_not_ready") => "browser_not_ready",
                Some("browser_closed") => "browser_closed",
                Some("route_changed") => "route_changed",
                Some("display_failed") => "display_failed",
                Some("limit_exceeded") => "limit_exceeded",
                Some("timeout") => "timeout",
                Some("invalid_params") => "invalid_params",
                _ => "upstream_error",
            };
            return Err(Failure::new(
                code,
                "selected core rejected the browser request",
            ));
        }
        let result = value.get_mut("result").ok_or(BAD_RESPONSE)?.take();
        if !result.is_object() {
            return Err(BAD_RESPONSE);
        }
        Ok(result)
    }

    fn local_request(
        path: &Path,
        method: &str,
        params: Value,
        owner: Option<&Arc<Owner>>,
        timeout: Duration,
    ) -> GatewayResult<Value> {
        if owner.is_some_and(|owner| owner.stop.stopped()) {
            return Err(CANCELLED);
        }
        let deadline = Instant::now() + timeout.min(RPC_TIMEOUT);
        let id = random_id()?;
        let mut connection = connect(path, deadline)?;
        let _guard = owner.map(|owner| owner.track(&connection)).transpose()?;
        write_request(&mut connection, &id, method, params, deadline)?;
        let frame =
            api::read_response_frame_with_deadline(&mut connection, remaining(deadline)?)
                .map_err(|_| Failure::new("unavailable", "local browser API response failed"))?;
        response(&frame, &id)
    }

    fn browser_result(value: &Value, owner: &Owner, method: &str) -> GatewayResult<()> {
        if value.get("type").and_then(Value::as_str) != Some("browser_result")
            || value.get("browser_id").and_then(Value::as_str) != Some(owner.browser.as_str())
            || value.get("method").and_then(Value::as_str) != Some(method)
        {
            return Err(BAD_RESPONSE);
        }
        Ok(())
    }

    fn generation(value: &Value) -> GatewayResult<u64> {
        value
            .as_u64()
            .or_else(|| {
                value.as_str().and_then(|value| {
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        None
                    } else {
                        value.parse().ok()
                    }
                })
            })
            .ok_or(BAD_RESPONSE)
    }

    fn prepare(
        owner: Arc<Owner>,
        ttl: u64,
        expires_at: u64,
        output: mpsc::Sender<Output>,
    ) -> GatewayResult<u64> {
        let result = local_request(
            &owner.socket,
            "browser.control.acquire",
            json!({"browser":owner.browser,"ttl_s":ttl}),
            None,
            RPC_TIMEOUT,
        )?;
        let secret = result
            .get("lease")
            .and_then(Value::as_str)
            .filter(|secret| !secret.is_empty() && secret.len() <= 128)
            .ok_or(BAD_RESPONSE)?;
        // Arm cleanup before validating any remaining startup metadata.
        *owner
            .lease
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Lease {
            secret: secret.to_owned(),
        });
        browser_result(&result, &owner, "browser.control.acquire")?;
        let lease_generation = generation(&result["lease_generation"])?;
        let expires_at = result
            .get("expires_at")
            .and_then(Value::as_u64)
            .map_or(expires_at, |lease_expiry| expires_at.min(lease_expiry));
        if owner.stop.stopped() {
            return Err(CANCELLED);
        }
        if expires_at <= unix_seconds()? {
            return Err(Failure::new(
                "timeout",
                "browser control lease expired during startup",
            ));
        }

        let deadline = Instant::now() + RPC_TIMEOUT;
        let mut connection = connect(&owner.socket, deadline)?;
        let guard = owner.track(&connection)?;
        let id = random_id()?;
        write_request(
            &mut connection,
            &id,
            "browser.cdp.subscribe",
            owner.params()?,
            deadline,
        )?;
        // Unlike the ordinary response helper, this keeps bytes following the
        // ack's LF. The owner may put ack and the first event in one OS read.
        let mut reader = BufReader::new(connection);
        let ack = api::read_stream_frame_with_deadline(&mut reader, deadline)
            .map_err(|_| BAD_RESPONSE)?
            .ok_or(BAD_RESPONSE)?;
        let ack = response(&ack, &id)?;
        let subscription = &ack["subscription_id"];
        if !(subscription.as_u64().is_some()
            || subscription
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.len() <= 128))
            || ack.get("browser_id").and_then(Value::as_str) != Some(owner.browser.as_str())
            || generation(&ack["lease_generation"])? != lease_generation
        {
            return Err(BAD_RESPONSE);
        }
        reader
            .get_ref()
            .clear_timeouts()
            .map_err(|_| BAD_RESPONSE)?;
        if let Some(root) = ack["owner_workspace_root"].as_str() {
            // Failure to pin the workspace disables file access, not page control.
            if let Ok(files) = crate::browser::files::WorkspaceFiles::open(Path::new(root)) {
                let _ = owner.files.set(files);
            }
        }
        let stream_owner = Arc::clone(&owner);
        let thread = std::thread::Builder::new()
            .name("luvus-cdp-events".into())
            .spawn(move || {
                let _guard = guard;
                let result = receive_events(reader, &stream_owner, output);
                stream_owner.stop.finish(result);
            })
            .map_err(|_| Failure::new("unavailable", "cannot start CDP event reader"))?;
        *owner
            .reader
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(thread);

        let result = local_request(
            &owner.socket,
            "browser.cdp.connect",
            owner.params()?,
            Some(&owner),
            RPC_TIMEOUT,
        )?;
        browser_result(&result, &owner, "browser.cdp.connect")?;
        Ok(expires_at)
    }

    #[derive(Default)]
    struct Budget(AtomicUsize);

    struct Reservation {
        budget: Arc<Budget>,
        bytes: usize,
    }

    impl Reservation {
        fn new(budget: Arc<Budget>) -> Self {
            Self { budget, bytes: 0 }
        }

        fn grow(&mut self, bytes: usize) -> GatewayResult<()> {
            let mut used = self.budget.0.load(Ordering::Acquire);
            loop {
                let next = used
                    .checked_add(bytes)
                    .filter(|used| *used <= MAX_MESSAGE)
                    .ok_or(OVERFLOW)?;
                match self.budget.0.compare_exchange_weak(
                    used,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(current) => used = current,
                }
            }
            self.bytes += bytes;
            Ok(())
        }
    }

    impl Drop for Reservation {
        fn drop(&mut self) {
            self.budget.0.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }

    struct Output {
        text: String,
        reservation: Reservation,
    }

    struct Partial {
        next: usize,
        total: usize,
        output: Output,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Event {
        event: String,
        data: Chunk,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Chunk {
        transfer: String,
        index: usize,
        total: usize,
        text: String,
    }

    fn valid_object(text: &str) -> GatewayResult<()> {
        if text.len() > MAX_MESSAGE || !text.trim_start().starts_with('{') {
            return Err(BAD_CHUNK);
        }
        // Validate without expanding a 64 MiB JSON payload into a Value tree.
        serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|_| BAD_CHUNK)?;
        Ok(())
    }

    fn receive_events(
        mut reader: BufReader<Conn>,
        owner: &Arc<Owner>,
        output: mpsc::Sender<Output>,
    ) -> GatewayResult<()> {
        let stop = &owner.stop;
        let budget = Arc::new(Budget::default());
        let mut downloads = super::downloads::Receiver::default();
        let mut partials: HashMap<String, Partial> = HashMap::new();
        let mut seen = HashSet::new();
        while !stop.stopped() {
            let Some(frame) = api::read_stream_frame(&mut reader)
                .map_err(|_| Failure::new("disconnected", "browser CDP subscription failed"))?
            else {
                return if partials.is_empty() {
                    Ok(())
                } else {
                    Err(BAD_CHUNK)
                };
            };
            if frame.len() >= MAX_API_FRAME {
                return Err(BAD_CHUNK);
            }
            let event: Event = serde_json::from_str(&frame).map_err(|_| BAD_CHUNK)?;
            let chunk = event.data;
            if event.event != "browser.cdp"
                || !opaque_id(&chunk.transfer)
                || chunk.total == 0
                || chunk.total > MAX_CHUNKS
                || chunk.index >= chunk.total
                || chunk.text.is_empty()
                || chunk.text.len() > OUTPUT_CHUNK
            {
                return Err(BAD_CHUNK);
            }
            if !partials.contains_key(&chunk.transfer) {
                if chunk.index != 0 || seen.contains(&chunk.transfer) {
                    return Err(BAD_CHUNK);
                }
                if partials.len() >= MAX_TRANSFERS || seen.len() >= MAX_SEEN_TRANSFERS {
                    return Err(OVERFLOW);
                }
                seen.insert(chunk.transfer.clone());
                partials.insert(
                    chunk.transfer.clone(),
                    Partial {
                        next: 0,
                        total: chunk.total,
                        output: Output {
                            text: String::new(),
                            reservation: Reservation::new(Arc::clone(&budget)),
                        },
                    },
                );
            }
            let partial = partials.get_mut(&chunk.transfer).ok_or(BAD_CHUNK)?;
            if partial.next != chunk.index || partial.total != chunk.total {
                return Err(BAD_CHUNK);
            }
            partial.output.reservation.grow(chunk.text.len())?;
            // Reserve exactly rather than allowing geometric growth past the
            // byte budget. Only two strings may be under construction at once.
            partial
                .output
                .text
                .try_reserve_exact(chunk.text.len())
                .map_err(|_| OVERFLOW)?;
            partial.output.text.push_str(&chunk.text);
            partial.next += 1;
            if partial.next == partial.total {
                let completed = partials.remove(&chunk.transfer).ok_or(BAD_CHUNK)?.output;
                valid_object(&completed.text)?;
                // Only native download records can publish bytes. An ordinary CDP
                // event, including one with a page-controlled URL, has no file authority.
                if completed.text.starts_with("{\"download\":") {
                    let output = output.clone();
                    let owner_for_event = Arc::clone(owner);
                    let budget = Arc::clone(&budget);
                    let emit = move |event: Value| {
                        let text = event.to_string();
                        let mut reservation = Reservation::new(Arc::clone(&budget));
                        if reservation.grow(text.len()).is_err()
                            || output.try_send(Output { text, reservation }).is_err()
                        {
                            owner_for_event.stop.finish(Err(OVERFLOW));
                        }
                    };
                    downloads.receive(&owner.downloads, &completed.text, emit)?;
                    continue;
                }
                // Budget ownership travels through the channel and WS write.
                // Overflow fails closed; this thread never waits on a slow WS.
                output.try_send(completed).map_err(|_| OVERFLOW)?;
            }
        }
        Ok(())
    }

    struct WebState {
        owner: Arc<Owner>,
        host: String,
        path: String,
        // 0 = available, 1 = validated upgrade pending, 2 = consumed.
        connection: AtomicU8,
        output: Mutex<Option<mpsc::Receiver<Output>>>,
    }

    async fn serve(owner: Arc<Owner>, ttl: u64, expires_at: u64) -> GatewayResult<()> {
        let (sender, receiver) = mpsc::channel(OUTPUT_QUEUE);
        let worker_owner = Arc::clone(&owner);
        let expires_at =
            tokio::task::spawn_blocking(move || prepare(worker_owner, ttl, expires_at, sender))
                .await
                .map_err(|_| Failure::new("unavailable", "CDP gateway startup worker failed"))??;
        if owner.stop.stopped() {
            return owner.stop.wait().await;
        }
        if expires_at <= unix_seconds()? {
            return Err(Failure::new(
                "timeout",
                "browser control lease expired during startup",
            ));
        }
        let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(|_| Failure::new("unavailable", "cannot bind loopback CDP listener"))?;
        let port = listener
            .local_addr()
            .map_err(|_| Failure::new("unavailable", "cannot resolve loopback CDP listener"))?
            .port();
        let host = format!("127.0.0.1:{port}");
        let path = format!("/{}", random_id()?);
        let descriptor = json!({
            "type":"browser_cdp", "browser_id":owner.browser,
            "endpoint":format!("ws://{host}{path}"), "expires_at":expires_at,
        });
        let state = Arc::new(WebState {
            owner,
            host,
            path,
            connection: AtomicU8::new(0),
            output: Mutex::new(Some(receiver)),
        });
        let router = Router::new().fallback(handshake).with_state(state);
        {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{descriptor}")
                .and_then(|()| stdout.flush())
                .map_err(|_| {
                    Failure::new("output_failed", "cannot write CDP gateway descriptor")
                })?;
        }
        // Dropping this future closes the listener. Upgraded sockets additionally
        // observe Stop and are dropped when the dedicated runtime is destroyed.
        tokio::select! {
            result = async { axum::serve(listener, router).await } => {
                result.map_err(|_| Failure::new("unavailable", "CDP gateway listener failed"))
            }
            _ = tokio::time::sleep(Duration::from_secs(expires_at.saturating_sub(unix_seconds()?))) => Ok(()),
        }
    }

    fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
        let mut values = headers.get_all(name).iter();
        let value = values.next()?.to_str().ok()?;
        if values.next().is_some() {
            return None;
        }
        Some(value)
    }

    fn valid_handshake(request: &Request, state: &WebState) -> bool {
        let headers = request.headers();
        let key = single_header(headers, header::SEC_WEBSOCKET_KEY).unwrap_or("");
        request.method() == Method::GET
            && request.uri().authority().is_none()
            && request.uri().scheme().is_none()
            && request.uri().path() == state.path
            && request.uri().query().is_none()
            && !headers.contains_key(header::ORIGIN)
            && single_header(headers, header::HOST) == Some(state.host.as_str())
            && !headers.contains_key(header::TRANSFER_ENCODING)
            && (!headers.contains_key(header::CONTENT_LENGTH)
                || single_header(headers, header::CONTENT_LENGTH) == Some("0"))
            && single_header(headers, header::UPGRADE)
                .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
            && single_header(headers, header::CONNECTION).is_some_and(|value| {
                value.split(',').any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
            && single_header(headers, header::SEC_WEBSOCKET_VERSION) == Some("13")
            // Canonical base64 of a 16-byte nonce, without needing a new crate.
            && key.len() == 24
            && key.ends_with("==")
            && key.as_bytes()[..22]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'+' | b'/'))
            && matches!(key.as_bytes()[21], b'A' | b'Q' | b'g' | b'w')
    }

    async fn handshake(State(state): State<Arc<WebState>>, request: Request) -> Response {
        if state.owner.stop.stopped() || !valid_handshake(&request, &state) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let (mut parts, _) = request.into_parts();
        let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(upgrade) => upgrade,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        if state
            .connection
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return StatusCode::CONFLICT.into_response();
        }
        let failed = Arc::clone(&state);
        upgrade
            .max_message_size(MAX_MESSAGE)
            // Playwright may send an entire large command in a single WS frame.
            .max_frame_size(MAX_MESSAGE)
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_MESSAGE + MAX_API_FRAME)
            .on_failed_upgrade(move |_| {
                let _ =
                    failed
                        .connection
                        .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire);
            })
            .on_upgrade(move |socket| async move {
                state.connection.store(2, Ordering::Release);
                client(socket, state).await;
            })
    }

    struct ClientEnd(Arc<Owner>);

    impl Drop for ClientEnd {
        fn drop(&mut self) {
            self.0.stop.finish(Ok(()));
            self.0.cancel_connections();
        }
    }

    async fn client(socket: WebSocket, state: Arc<WebState>) {
        let _end = ClientEnd(Arc::clone(&state.owner));
        let output = state
            .output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some(output) = output else {
            state.owner.stop.finish(Err(CANCELLED));
            return;
        };
        let (sink, source) = socket.split();
        let (controls, control_rx) = mpsc::channel(4);
        let result = tokio::select! {
            biased;
            result = state.owner.stop.wait() => result,
            result = read_socket(source, Arc::clone(&state.owner), controls) => result,
            result = write_socket(sink, output, control_rx) => result,
        };
        state.owner.stop.finish(result);
    }

    async fn read_socket(
        mut source: SplitStream<WebSocket>,
        owner: Arc<Owner>,
        controls: mpsc::Sender<Message>,
    ) -> GatewayResult<()> {
        let budget = Arc::new(Budget::default());
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                completed = workers.join_next(), if !workers.is_empty() => {
                    completed.ok_or(CANCELLED)?
                        .map_err(|_| Failure::new("unavailable", "CDP transfer worker failed"))??;
                }
                // Admission preserves wire order; display-side command completion stays asynchronous.
                message = source.next(), if workers.is_empty() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let mut reservation = Reservation::new(Arc::clone(&budget));
                            reservation.grow(text.len())?;
                            let owner = Arc::clone(&owner);
                            let replies = controls.clone();
                            workers.spawn_blocking(move || {
                                let _reservation = reservation;
                                forward(owner, text, replies)
                            });
                        }
                        Some(Ok(Message::Ping(data))) => {
                            controls.try_send(Message::Pong(data)).map_err(|_| OVERFLOW)?;
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(_))) | None => return Ok(()),
                        Some(Ok(Message::Binary(_))) => {
                            return Err(Failure::new("invalid_cdp", "CDP messages must be JSON text"));
                        }
                        Some(Err(_)) => {
                            return Err(Failure::new("disconnected", "CDP WebSocket read failed"));
                        }
                    }
                }
            }
        }
    }

    #[derive(Deserialize)]
    struct CommandHeader<'a> {
        id: i64,
        method: &'a str,
    }

    fn chunk_end(text: &str, start: usize) -> usize {
        let mut end = (start + INPUT_CHUNK).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        end
    }

    fn forward(
        owner: Arc<Owner>,
        text: Utf8Bytes,
        replies: mpsc::Sender<Message>,
    ) -> GatewayResult<()> {
        valid_object(text.as_str()).map_err(|_| {
            Failure::new("invalid_cdp", "CDP command must be a bounded JSON object")
        })?;
        let command: CommandHeader = serde_json::from_str(text.as_str())
            .map_err(|_| Failure::new("invalid_cdp", "CDP command requires an id and method"))?;
        if matches!(
            command.method,
            "DOM.setFileInputFiles" | "Browser.setDownloadBehavior"
        ) || command.method.starts_with("Luvus.")
        {
            if text.len() > 64 * 1024 {
                return Err(OVERFLOW);
            }
            let mut envelope: Value =
                serde_json::from_str(text.as_str()).map_err(|_| BAD_RESPONSE)?;
            let result = if command.method == "DOM.setFileInputFiles" {
                prepare_upload(&owner, &mut envelope)
            } else if command.method == "Browser.setDownloadBehavior" {
                owner.downloads.configure(owner.files.get(), &mut envelope)
            } else {
                Err(Failure::new("unsupported", "private gateway command"))
            };
            match result {
                Ok(()) => return send_text(&owner, &envelope.to_string()),
                Err(error) => {
                    let mut reply =
                        json!({"id":command.id,"error":{"code":-32000,"message":error.code}});
                    if let Some(session) =
                        envelope["sessionId"].as_str().filter(|id| id.len() <= 256)
                    {
                        reply["sessionId"] = json!(session);
                    }
                    return replies
                        .blocking_send(Message::Text(reply.to_string().into()))
                        .map_err(|_| CANCELLED);
                }
            }
        }
        send_text(&owner, text.as_str())
    }

    fn prepare_upload(owner: &Arc<Owner>, envelope: &mut Value) -> GatewayResult<()> {
        const INVALID: Failure = Failure::new("invalid_params", "invalid file input command");
        const DENIED: Failure = Failure::new("file_denied", "owner file unavailable");
        if envelope["sessionId"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 256)
        {
            return Err(INVALID);
        }
        let params = envelope["params"].as_object_mut().ok_or(INVALID)?;
        if params.keys().any(|key| {
            !matches!(
                key.as_str(),
                "files" | "nodeId" | "backendNodeId" | "objectId"
            )
        }) || params.len() != 2
        {
            return Err(INVALID);
        }
        let paths = params
            .get("files")
            .and_then(Value::as_array)
            .ok_or(INVALID)?;
        if paths.len() > 16 {
            return Err(OVERFLOW);
        }
        let mut files = Vec::with_capacity(paths.len());
        let mut remaining = MAX_MESSAGE;
        // Validate the entire batch before creating any display-side staging files.
        for path in paths {
            let path = Path::new(path.as_str().filter(|s| s.len() <= 4096).ok_or(INVALID)?);
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| {
                    s.len() <= 255
                        && !s.contains(['/', '\\', ':'])
                        && !s.chars().any(char::is_control)
                })
                .ok_or(INVALID)?;
            let bytes = owner
                .files
                .get()
                .ok_or(DENIED)?
                .read(path, remaining, || owner.stop.stopped())
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::InvalidData => OVERFLOW,
                    std::io::ErrorKind::Interrupted => CANCELLED,
                    _ => DENIED,
                })?;
            remaining -= bytes.len();
            files.push((name.to_owned(), bytes));
        }
        let mut handles = Vec::with_capacity(files.len());
        for (name, bytes) in files {
            let handle = random_id()?;
            // Each record fits one existing CDP input chunk, including the UTF-8 name.
            let mut offset = 0;
            loop {
                let end = (offset + 5 * 1024).min(bytes.len());
                let stage = json!({"stage":{"handle":handle,"name":name,"size":bytes.len(),
                    "offset":offset,"data":crate::base64_encode(&bytes[offset..end])}});
                send_text(owner, &stage.to_string())?;
                if end == bytes.len() {
                    break;
                }
                offset = end;
            }
            handles.push(handle);
        }
        params.insert("files".into(), json!(handles));
        envelope["method"] = json!("Luvus.setFileInputFiles");
        Ok(())
    }

    fn send_text(owner: &Arc<Owner>, text: &str) -> GatewayResult<()> {
        let transfer = random_id()?;
        let mut total = 0;
        let mut start = 0;
        while start < text.len() {
            start = chunk_end(text, start);
            total += 1;
        }
        if total == 0 || total > MAX_CHUNKS {
            return Err(OVERFLOW);
        }
        start = 0;
        for index in 0..total {
            if owner.stop.stopped() {
                return Err(CANCELLED);
            }
            let end = chunk_end(text, start);
            let mut params = owner.params()?;
            params["chunk"] = json!({
                "transfer":transfer,"index":index,"total":total,"text":&text[start..end],
            });
            let result = local_request(
                &owner.socket,
                "browser.cdp.send",
                params,
                Some(owner),
                RPC_TIMEOUT,
            )?;
            browser_result(&result, owner, "browser.cdp.send")?;
            start = end;
        }
        Ok(())
    }

    async fn write_socket(
        mut sink: SplitSink<WebSocket, Message>,
        mut output: mpsc::Receiver<Output>,
        mut controls: mpsc::Receiver<Message>,
    ) -> GatewayResult<()> {
        loop {
            let (message, reservation) = tokio::select! {
                control = controls.recv() => {
                    let Some(control) = control else { return Ok(()); };
                    (control, None)
                }
                completed = output.recv() => {
                    let Some(completed) = completed else { return Ok(()); };
                    (Message::Text(completed.text.into()), Some(completed.reservation))
                }
            };
            tokio::time::timeout(WRITE_TIMEOUT, sink.send(message))
                .await
                .map_err(|_| Failure::new("timeout", "CDP WebSocket write deadline expired"))?
                .map_err(|_| Failure::new("disconnected", "CDP WebSocket write failed"))?;
            drop(reservation);
        }
    }
}
