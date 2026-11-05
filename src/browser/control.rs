//! Ephemeral, single-owner browser command broker. No networking or page execution
//! happens here. Only a trusted display adapter may register/complete requests.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{
    mpsc::{Sender, SyncSender},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::event::AppEvent;
use crate::ipc::api::ApiRequest;
use crate::ipc::app_surface::protocol::{CdpChunk, Target};

pub const MAX_PER_DISPLAY: usize = 8;
pub const MAX_BROWSERS: usize = 64;
pub const MAX_PENDING: usize = 32;
pub const MAX_PARAMS_BYTES: usize = 64 * 1024;
// Leave room for the public API envelope and identifiers, including escaping.
pub const MAX_RESULT_BYTES: usize = crate::terminal::backend::MAX_FRAME_BYTES - 4096;
pub const DEFAULT_TTL_S: u64 = 300;
pub const MAX_TTL_S: u64 = 3600;
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 20_000;

pub const METHODS: &[&str] = &[
    "browser.list",
    "browser.get",
    "browser.navigate",
    "browser.back",
    "browser.forward",
    "browser.reload",
    "browser.close",
    "browser.snapshot",
    "browser.act",
    "browser.evaluate",
    "browser.screenshot",
    "browser.wait",
    "browser.dialog",
    "browser.control.acquire",
    "browser.control.release",
    "browser.control.status",
    "browser.cdp.connect",
    "browser.cdp.send",
    "browser.cdp.subscribe",
];

pub fn handles(method: &str) -> bool {
    method != "browser.cdp.subscribe" && METHODS.contains(&method)
}

pub fn is_read(method: &str) -> bool {
    matches!(
        method,
        "browser.list" | "browser.get" | "browser.control.status"
    )
}

fn opaque_id() -> Result<String, String> {
    crate::terminal::backend::random_id()
}

fn same_target(a: &Target, b: &Target) -> bool {
    a.generation == b.generation
        && a.workspace_id == b.workspace_id
        && a.tab_id == b.tab_id
        && a.pane_id == b.pane_id
        && a.terminal_id == b.terminal_id
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Preparing,
    Ready,
    Failed,
    Closed,
    Revoked,
}
impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Closed => "closed",
            Self::Revoked => "revoked",
        }
    }
    fn live(self) -> bool {
        matches!(self, Self::Preparing | Self::Ready)
    }
}

struct Lease {
    secret: String,
    live: Arc<AtomicBool>,
    deadline: Instant,
    expires_at: u64,
    ready: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
    }
}

struct CdpSubscription {
    id: String,
    generation: u64,
    sender: SyncSender<String>,
    active: Arc<AtomicBool>,
}

impl Drop for CdpSubscription {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

struct Browser {
    client_id: u64,
    target: Target,
    url: String,
    state: State,
    generation: u64,
    lease: Option<Lease>,
    cdp: Option<CdpSubscription>,
}

#[derive(Clone, Copy, PartialEq)]
enum Operation {
    Launch,
    Acquire,
    Release,
    Close,
    Command,
}

struct Pending {
    browser_id: String,
    client_id: u64,
    target: Target,
    generation: u64,
    deadline: Instant,
    operation: Operation,
    method: String,
    url: Option<String>,
    reply: Option<(String, Sender<String>)>,
    screenshot: Option<super::files::Screenshot>,
}

/// Owned only by App, never persisted. The optional timer owns no application
/// state: it emits one wake at the next deadline, then parks until re-armed.
#[derive(Default)]
pub struct BrowserRegistry {
    screenshot_slots: Arc<AtomicUsize>,
    browsers: BTreeMap<String, Browser>,
    pending: HashMap<String, Pending>,
    commands: Vec<(u64, Value)>,
    revocations: BTreeMap<String, (u64, Value)>,
    app_tx: Option<Sender<AppEvent>>,
    wake: Option<DeadlineWake>,
}

impl BrowserRegistry {
    pub fn new(app_tx: Sender<AppEvent>) -> Self {
        Self {
            app_tx: Some(app_tx),
            ..Self::default()
        }
    }

    pub fn register(
        &mut self,
        client_id: u64,
        target: Target,
        url: String,
    ) -> Result<(String, Value), String> {
        self.expire();
        // Reuse the target's protocol validator without weakening its boundary.
        crate::ipc::app_surface::protocol::Request::Focus {
            id: "browser".into(),
            target: target.clone(),
            activate: false,
        }
        .validate()
        .map_err(str::to_owned)?;
        if !valid_url(&url) {
            return Err("invalid_params".into());
        }
        self.browsers.retain(|_, b| b.state.live());
        if self.browsers.len() >= MAX_BROWSERS
            || self
                .browsers
                .values()
                .filter(|b| b.client_id == client_id)
                .count()
                >= MAX_PER_DISPLAY
            || self.pending.len() >= MAX_PENDING
            || self.revocations.len() >= MAX_BROWSERS
        {
            return Err("limit_exceeded".into());
        }
        self.ensure_timer()?;
        let browser_id = opaque_id()?;
        let request_id = opaque_id()?;
        self.pending.insert(
            request_id.clone(),
            Pending {
                browser_id: browser_id.clone(),
                client_id,
                target: target.clone(),
                generation: 0,
                deadline: Instant::now() + Duration::from_millis(MAX_TIMEOUT_MS),
                operation: Operation::Launch,
                method: "browser.launch".into(),
                url: None,
                reply: None,
                screenshot: None,
            },
        );
        self.browsers.insert(
            browser_id.clone(),
            Browser {
                client_id,
                target: target.clone(),
                url: url.clone(),
                state: State::Preparing,
                generation: 0,
                lease: None,
                cdp: None,
            },
        );
        self.arm();
        Ok((
            browser_id.clone(),
            json!({"request_id":request_id,"browser_id":browser_id,
            "target":target,"url":url,"lease_generation":"0"}),
        ))
    }

    pub fn subscribe_cdp(
        &mut self,
        browser_id: &str,
        lease: &str,
        subscription_id: String,
        sender: SyncSender<String>,
        active: Arc<AtomicBool>,
    ) -> Result<Value, String> {
        self.expire();
        let browser = self
            .browsers
            .get_mut(browser_id)
            .ok_or("browser_not_found")?;
        if !active.load(Ordering::Acquire)
            || browser.state != State::Ready
            || !browser.lease.as_ref().is_some_and(|current| {
                current.ready && current.secret == lease && current.deadline > Instant::now()
            })
        {
            return Err("control_required".into());
        }
        if browser.cdp.is_some() {
            return Err("control_busy".into());
        }
        let result = json!({"subscription_id":subscription_id,"browser_id":browser_id,
            "lease_generation":browser.generation.to_string()});
        browser.cdp = Some(CdpSubscription {
            id: subscription_id,
            generation: browser.generation,
            sender,
            active,
        });
        Ok(result)
    }

    pub fn unsubscribe_cdp(&mut self, browser_id: &str, subscription_id: &str) -> bool {
        if !self
            .browsers
            .get(browser_id)
            .and_then(|browser| browser.cdp.as_ref())
            .is_some_and(|subscription| subscription.id == subscription_id)
        {
            return false;
        }
        self.revoke_control(browser_id, "control_released", false);
        self.arm();
        true
    }

    pub fn cdp_event(
        &mut self,
        client_id: u64,
        browser_id: &str,
        target: &Target,
        generation: u64,
        chunk: CdpChunk,
    ) -> bool {
        let Some(browser) = self.browsers.get(browser_id) else {
            return false;
        };
        if browser.client_id != client_id
            || browser.generation != generation
            || !same_target(&browser.target, target)
            || browser.state != State::Ready
            || !browser
                .lease
                .as_ref()
                .is_some_and(|lease| lease.ready && lease.deadline > Instant::now())
        {
            return false;
        }
        let Some(subscription) = browser.cdp.as_ref().filter(|subscription| {
            subscription.generation == generation && subscription.active.load(Ordering::Acquire)
        }) else {
            return false;
        };
        if subscription
            .sender
            .try_send(json!({"event":"browser.cdp","data":chunk}).to_string())
            .is_ok()
        {
            return true;
        }
        self.revoke_control(browser_id, "control_revoked", false);
        self.arm();
        false
    }

    pub fn target(&self, browser_id: &str) -> Option<&Target> {
        self.browsers.get(browser_id).map(|b| &b.target)
    }

    pub fn bindings(&self) -> impl Iterator<Item = (&str, u64, &Target)> {
        self.browsers
            .iter()
            .filter(|(_, b)| b.state.live())
            .map(|(id, b)| (id.as_str(), b.client_id, &b.target))
    }

    pub fn get(&self, browser_id: &str) -> Option<Value> {
        let b = self.browsers.get(browser_id)?;
        let lease = b.lease.as_ref().filter(|l| l.deadline > Instant::now());
        Some(
            json!({"type":"browser_session","browser_id":browser_id,"target":b.target,"url":b.url,
            "state":b.state.name(),"lease_generation":b.generation.to_string(),
            "control":{"state":match lease { Some(l) if l.ready => "acquired", Some(_) => "preparing", None => "uncontrolled" },
                "expires_at":lease.map(|l| l.expires_at)}}),
        )
    }

    pub fn list(&self) -> Vec<Value> {
        self.browsers.keys().filter_map(|id| self.get(id)).collect()
    }

    /// Completion uses an unguessable one-shot request ID; its stored exact
    /// target and generation must still match. Wrong displays cannot consume it.
    pub fn complete(
        &mut self,
        client_id: u64,
        request_id: &str,
        result: Result<Value, String>,
    ) -> bool {
        self.expire();
        let Some(p) = self.pending.get(request_id) else {
            return false;
        };
        if p.client_id != client_id
            || !self.browsers.get(&p.browser_id).is_some_and(|b| {
                b.client_id == client_id
                    && b.generation == p.generation
                    && same_target(&b.target, &p.target)
            })
        {
            return false;
        }
        let p = self.pending.remove(request_id).expect("checked pending");
        self.commands.retain(|(_, v)| v["request_id"] != request_id);
        let result = match result {
            Ok(value) if encoded_len(&value) <= MAX_RESULT_BYTES => Ok(value),
            Ok(_) => Err("limit_exceeded"),
            // Only canonical codes cross this boundary; never reflect host
            // errors containing credentials, native paths, or proxy details.
            Err(error) => Err(display_error(&error)),
        };
        let revoked = result.as_ref().err().copied().filter(|code| {
            matches!(
                *code,
                "route_changed"
                    | "browser_closed"
                    | "control_revoked"
                    | "control_denied"
                    | "timeout"
            )
        });
        let b = self
            .browsers
            .get_mut(&p.browser_id)
            .expect("checked browser");
        match (&result, p.operation) {
            (Ok(_), Operation::Launch) => b.state = State::Ready,
            (Err(_), Operation::Launch) => b.state = State::Failed,
            (Ok(_), Operation::Acquire) => {
                if let Some(lease) = &mut b.lease {
                    lease.ready = true;
                }
            }
            (Err(_), Operation::Acquire) => {
                b.lease = None;
            }
            (Ok(_), Operation::Close) => {
                b.state = State::Closed;
                b.lease = None;
            }
            _ => {}
        }
        match revoked {
            Some("route_changed") => b.state = State::Revoked,
            Some("browser_closed") => b.state = State::Closed,
            _ => {}
        }
        if result.is_ok() {
            if let Some(url) = &p.url {
                b.url = url.clone();
            }
        }
        let response = match result {
            Ok(data) => {
                let mut value = json!({"type":"browser_result","browser_id":p.browser_id,
                    "request_id":request_id,"method":p.method,
                    "state":if p.operation == Operation::Launch { "ready" } else { "completed" }});
                if p.operation == Operation::Acquire {
                    let lease = b.lease.as_ref().expect("acquiring lease");
                    value["lease"] = json!(lease.secret);
                    value["expires_at"] = json!(lease.expires_at);
                    value["lease_generation"] = json!(b.generation.to_string());
                } else if p.operation == Operation::Command {
                    value["data"] = data;
                }
                Ok(value)
            }
            Err(code) => Err(code),
        };
        match (p.screenshot, response, p.reply) {
            (Some(screenshot), Ok(mut result), Some(original_reply)) => {
                let worker_reply = original_reply.clone();
                let spawn = std::thread::Builder::new()
                    .name("luvus-browser-screenshot".into())
                    .spawn(move || {
                        let saved = screenshot.save(&result["data"]).map(|data| {
                            result["data"] = data;
                            result
                        });
                        reply(Some(worker_reply), saved);
                    });
                if spawn.is_err() {
                    reply(Some(original_reply), Err("unavailable"));
                }
            }
            (_, response, reply_to) => reply(reply_to, response),
        }
        if let Some(code) = revoked {
            self.revoke_control(
                &p.browser_id,
                code,
                p.operation == Operation::Launch
                    || matches!(code, "route_changed" | "browser_closed"),
            );
        } else if p.operation == Operation::Close && b.state == State::Closed {
            self.revoke_control(&p.browser_id, "browser_closed", false);
        } else if matches!(p.operation, Operation::Launch | Operation::Acquire)
            && response_is_error(&self.browsers, &p.browser_id, p.operation)
        {
            self.revoke_control(
                &p.browser_id,
                "display_failed",
                p.operation == Operation::Launch,
            );
        }
        self.arm();
        true
    }

    /// Prefer this at adapters whose result envelope carries the original fence.
    pub fn complete_bound(
        &mut self,
        client_id: u64,
        request_id: &str,
        target: &Target,
        lease_generation: u64,
        result: Result<Value, String>,
    ) -> bool {
        if !self
            .pending
            .get(request_id)
            .is_some_and(|p| same_target(&p.target, target) && p.generation == lease_generation)
        {
            return false;
        }
        self.complete(client_id, request_id, result)
    }

    /// Returns only trusted, bounded display commands, never lease secrets.
    /// Revocations precede later acquisitions/commands and are coalesced per tab.
    pub fn take_commands(&mut self) -> Vec<(u64, Value)> {
        let mut result: Vec<_> = std::mem::take(&mut self.revocations)
            .into_values()
            .collect();
        result.append(&mut self.commands);
        result
    }

    pub fn detach(&mut self, client_id: u64) {
        let ids: Vec<_> = self
            .browsers
            .iter()
            .filter(|(_, b)| b.client_id == client_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.revoke_control(&id, "display_detached", true);
            self.browsers.remove(&id);
            self.revocations.remove(&id);
        }
        self.arm();
    }

    /// Call on owner generation / topology invalidation, not on active-workspace
    /// changes. A hidden browser keeps its original target and valid lease.
    pub fn revoke(&mut self, browser_id: &str) {
        if let Some(b) = self.browsers.get_mut(browser_id) {
            b.state = State::Revoked;
        }
        self.revoke_control(browser_id, "route_changed", true);
        self.arm();
    }

    /// Trusted human/DevTools handover, not a public force-acquire operation.
    #[cfg(test)]
    pub fn human_takeover(&mut self, client_id: u64, browser_id: &str) -> bool {
        self.display_takeover(client_id, browser_id, None)
    }

    pub fn display_takeover(
        &mut self,
        client_id: u64,
        browser_id: &str,
        expected: Option<u64>,
    ) -> bool {
        if !self.browsers.get(browser_id).is_some_and(|b| {
            b.client_id == client_id
                && b.state == State::Ready
                && expected.is_none_or(|g| g == b.generation)
        }) {
            return false;
        }
        self.revoke_control(browser_id, "control_revoked", false);
        self.arm();
        true
    }

    #[cfg(test)]
    fn handle_api_request(&mut self, req: ApiRequest) {
        self.handle_owner_request(req, None);
    }

    pub fn handle_owner_request(&mut self, req: ApiRequest, root: Option<&std::path::Path>) {
        self.expire();
        let response = self.dispatch(&req, root);
        match response {
            Ok(Some(value)) => reply(Some((req.id, req.reply)), Ok(value)),
            Err(code) => reply(Some((req.id, req.reply)), Err(code)),
            Ok(None) => {} // parked: reply only after completion/revocation/deadline
        }
        self.arm();
    }

    fn dispatch(
        &mut self,
        req: &ApiRequest,
        root: Option<&std::path::Path>,
    ) -> Result<Option<Value>, &'static str> {
        validate_params(&req.method, &req.params).map_err(|_| "invalid_params")?;
        if req.method == "browser.list" {
            return Ok(Some(json!({"type":"browser_list","browsers":self.list()})));
        }
        let id = req.params["browser"].as_str().ok_or("invalid_params")?;
        let b = self.browsers.get(id).ok_or("browser_not_found")?;
        if is_read(&req.method) {
            return Ok(self.get(id));
        }
        match b.state {
            State::Ready => {}
            State::Preparing => return Err("browser_not_ready"),
            State::Closed => return Err("browser_closed"),
            State::Failed => return Err("display_failed"),
            State::Revoked => return Err("route_changed"),
        }
        let operation = match req.method.as_str() {
            "browser.control.acquire" => Operation::Acquire,
            "browser.control.release" => Operation::Release,
            "browser.close" => Operation::Close,
            _ => Operation::Command,
        };
        if operation != Operation::Release && self.pending.len() >= MAX_PENDING {
            return Err("limit_exceeded");
        }
        if operation == Operation::Acquire {
            if b.lease.is_some() || self.pending.values().any(|p| p.browser_id == id) {
                return Err("control_busy");
            }
        } else if !b.lease.as_ref().is_some_and(|lease| {
            lease.ready
                && Some(lease.secret.as_str()) == req.params["lease"].as_str()
                && lease.deadline > Instant::now()
        }) {
            return Err("control_required");
        }
        if req.method.starts_with("browser.cdp.")
            && !b.cdp.as_ref().is_some_and(|subscription| {
                subscription.generation == b.generation
                    && subscription.active.load(Ordering::Acquire)
            })
        {
            return Err("control_required");
        }
        let request_id = opaque_id().map_err(|_| "unavailable")?;
        let secret = if operation == Operation::Acquire {
            Some(opaque_id().map_err(|_| "unavailable")?)
        } else {
            None
        };
        self.ensure_timer().map_err(|_| "unavailable")?;
        if operation == Operation::Release {
            self.revoke_control(id, "control_released", false);
            // The explicit release below is itself the generation notification.
            self.revocations.remove(id);
        }
        let b = self.browsers.get_mut(id).ok_or("browser_not_found")?;
        let now = Instant::now();
        if let Some(secret) = secret {
            let ttl = req.params["ttl_s"].as_u64().unwrap_or(DEFAULT_TTL_S);
            b.generation += 1;
            b.lease = Some(Lease {
                secret,
                live: Arc::new(AtomicBool::new(true)),
                deadline: now + Duration::from_secs(ttl),
                expires_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    + ttl,
                ready: false,
            });
        }
        let timeout = req.params["timeout_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let mut params = req.params.clone();
        let object = params.as_object_mut().ok_or("invalid_params")?;
        object.remove("browser");
        object.remove("lease");
        let screenshot = if let Some(output) = object.remove("output") {
            let lease = b.lease.as_ref().ok_or("control_required")?;
            Some(super::files::Screenshot::reserve(
                root.ok_or("file_denied")?,
                output.as_str().ok_or("invalid_params")?,
                &self.screenshot_slots,
                &lease.live,
                lease.deadline.min(now + Duration::from_millis(timeout)),
            )?)
        } else {
            None
        };
        object.insert("timeout_ms".into(), json!(timeout));
        if operation == Operation::Acquire {
            object.insert(
                "ttl_s".into(),
                json!(req.params["ttl_s"].as_u64().unwrap_or(DEFAULT_TTL_S)),
            );
        }
        if req.method == "browser.screenshot" {
            object.entry("max_bytes").or_insert(json!(MAX_RESULT_BYTES));
        }
        self.commands.push((b.client_id, json!({"request_id":request_id,"browser_id":id,
            "method":req.method,"params":params,"lease_generation":b.generation.to_string(),"target":b.target})));
        self.pending.insert(
            request_id,
            Pending {
                browser_id: id.into(),
                client_id: b.client_id,
                target: b.target.clone(),
                generation: b.generation,
                deadline: now + Duration::from_millis(timeout),
                operation,
                method: req.method.clone(),
                url: req
                    .params
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                reply: Some((req.id.clone(), req.reply.clone())),
                screenshot,
            },
        );
        Ok(None)
    }

    fn revoke_control(&mut self, id: &str, code: &'static str, close: bool) {
        let Some(b) = self.browsers.get_mut(id) else {
            return;
        };
        b.generation += 1;
        b.lease = None;
        b.cdp = None;
        self.pending.retain(|_, p| {
            if p.browser_id != id {
                return true;
            }
            reply(p.reply.take(), Err(code));
            false
        });
        self.commands.retain(|(_, v)| v["browser_id"] != id);
        self.revocations.insert(id.into(), (b.client_id, json!({
            "request_id":format!("revoke:{id}:{}", b.generation),"browser_id":id,
            "method":if close { "browser.close" } else { "browser.control.release" },
            "params":{"reason":code},"lease_generation":b.generation.to_string(),"target":b.target
        })));
    }

    /// Only called for a deadline wake or an explicit broker operation; no ticks.
    pub fn expire(&mut self) {
        self.expire_at(Instant::now());
    }

    fn expire_at(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .browsers
            .iter()
            .filter(|(_, b)| b.lease.as_ref().is_some_and(|l| l.deadline <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.revoke_control(&id, "lease_expired", false);
        }
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let Some(p) = self.pending.remove(&id) else {
                continue;
            };
            reply(p.reply, Err("timeout"));
            self.commands.retain(|(_, v)| v["request_id"] != id);
            // An action can have already run; timeout is never a rollback claim.
            // Revoke its generation so late work cannot join a later controller.
            if p.operation == Operation::Launch {
                if let Some(b) = self.browsers.get_mut(&p.browser_id) {
                    b.state = State::Failed;
                }
            }
            self.revoke_control(&p.browser_id, "timeout", p.operation == Operation::Launch);
        }
        self.arm();
    }

    fn ensure_timer(&mut self) -> Result<(), String> {
        if self.wake.is_none() {
            if let Some(tx) = &self.app_tx {
                self.wake = Some(DeadlineWake::new(tx.clone())?);
            }
        }
        Ok(())
    }

    fn arm(&self) {
        if let Some(wake) = &self.wake {
            let deadline = self
                .pending
                .values()
                .map(|p| p.deadline)
                .chain(
                    self.browsers
                        .values()
                        .filter_map(|b| b.lease.as_ref().map(|l| l.deadline)),
                )
                .min();
            wake.arm(deadline);
        }
    }
}

fn display_error(error: &str) -> &'static str {
    match error {
        "timeout" => "timeout",
        "stale_reference" => "stale_reference",
        "route_changed" => "route_changed",
        "browser_closed" => "browser_closed",
        "control_revoked" => "control_revoked",
        "control_denied" => "control_denied",
        "unsupported" => "unsupported",
        "limit_exceeded" => "limit_exceeded",
        _ => "display_failed",
    }
}

fn response_is_error(browsers: &BTreeMap<String, Browser>, id: &str, operation: Operation) -> bool {
    browsers.get(id).is_some_and(|b| match operation {
        Operation::Launch => b.state == State::Failed,
        Operation::Acquire => b.lease.is_none(),
        _ => false,
    })
}

fn reply(reply: Option<(String, Sender<String>)>, result: Result<Value, &'static str>) {
    if let Some((id, tx)) = reply {
        let value = match result {
            Ok(result) => json!({"id":id,"result":result}),
            Err(code) => json!({"id":id,"error":{"code":code,"message":code}}),
        };
        let encoded = value.to_string();
        let encoded = if encoded.len().saturating_add(1)
            <= crate::terminal::backend::MAX_FRAME_BYTES
        {
            encoded
        } else {
            json!({"id":id,"error":{"code":"limit_exceeded","message":"browser reply exceeds frame budget"}}).to_string()
        };
        let _ = tx.send(encoded);
    }
}

fn encoded_len(value: &Value) -> usize {
    // The incoming transport already bounds allocation; check serialized bytes,
    // not scalar count, to account for quotes/control-character escaping.
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

fn valid_url(url: &str) -> bool {
    url == super::BLANK_URL || super::valid_url(url)
}
fn nonempty(value: &Value, max: usize) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= max)
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Act {
    Click {
        node: String,
        document_revision: String,
    },
    Fill {
        node: String,
        document_revision: String,
        text: String,
    },
    Press {
        node: String,
        document_revision: String,
        key: String,
    },
    Scroll {
        node: String,
        document_revision: String,
        x: i32,
        y: i32,
    },
    Select {
        node: String,
        document_revision: String,
        values: Vec<String>,
    },
}

fn valid_act(value: &Value) -> bool {
    let Ok(act) = serde_json::from_value::<Act>(value.clone()) else {
        return false;
    };
    let (node, revision, valid) = match act {
        Act::Click {
            node,
            document_revision,
        } => (node, document_revision, true),
        Act::Fill {
            node,
            document_revision,
            text,
        } => (node, document_revision, text.len() <= 32768),
        Act::Press {
            node,
            document_revision,
            key,
        } => (node, document_revision, !key.is_empty() && key.len() <= 128),
        Act::Scroll {
            node,
            document_revision,
            x,
            y,
        } => (
            node,
            document_revision,
            x.unsigned_abs() <= 100_000 && y.unsigned_abs() <= 100_000,
        ),
        Act::Select {
            node,
            document_revision,
            values,
        } => (
            node,
            document_revision,
            values.len() <= 100 && values.iter().all(|v| v.len() <= 1024),
        ),
    };
    valid && !node.is_empty() && node.len() <= 128 && !revision.is_empty() && revision.len() <= 128
}

/// Shared by CLI and broker: closed shapes and bounded JSON.
pub fn validate_params(method: &str, params: &Value) -> Result<(), String> {
    let invalid = || "invalid browser parameters".to_string();
    let object = params.as_object().ok_or_else(invalid)?;
    if method == "browser.cdp.subscribe" {
        return (object.len() == 2
            && ["browser", "lease"].iter().all(|key| {
                params[*key]
                    .as_str()
                    .is_some_and(crate::terminal::backend::valid_id)
            }))
        .then_some(())
        .ok_or_else(invalid);
    }
    if !handles(method) || encoded_len(params) > MAX_PARAMS_BYTES {
        return Err(invalid());
    }
    if method == "browser.list" {
        return if object.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    if !nonempty(&params["browser"], 128) {
        return Err(invalid());
    }
    let extra: &[&str] = match method {
        "browser.control.acquire" => &["ttl_s"],
        "browser.navigate" => &["url"],
        "browser.act" => &["action"],
        "browser.evaluate" => &["expression"],
        "browser.screenshot" => &["max_bytes", "output"],
        "browser.wait" => &["condition"],
        "browser.dialog" => &["accept", "prompt_text"],
        "browser.cdp.send" => &["chunk"],
        _ => &[],
    };
    if object.keys().any(|key| {
        key != "browser"
            && !(!is_read(method)
                && (key == "timeout_ms"
                    || (key == "lease" && method != "browser.control.acquire")
                    || extra.contains(&key.as_str())))
    }) {
        return Err(invalid());
    }
    if is_read(method) {
        return Ok(());
    }
    if method != "browser.control.acquire" && !nonempty(&params["lease"], 128) {
        return Err(invalid());
    }
    if object.contains_key("timeout_ms")
        && !params["timeout_ms"]
            .as_u64()
            .is_some_and(|v| (1..=MAX_TIMEOUT_MS).contains(&v))
    {
        return Err(invalid());
    }
    let valid = match method {
        "browser.control.acquire" => {
            !object.contains_key("ttl_s")
                || params["ttl_s"]
                    .as_u64()
                    .is_some_and(|v| (1..=MAX_TTL_S).contains(&v))
        }
        "browser.cdp.send" => serde_json::from_value::<CdpChunk>(params["chunk"].clone())
            .is_ok_and(|chunk| chunk.validate().is_ok() && chunk.text.len() <= 8192),
        "browser.navigate" => params["url"].as_str().is_some_and(valid_url),
        "browser.act" => valid_act(&params["action"]),
        "browser.evaluate" => nonempty(&params["expression"], 32768),
        "browser.screenshot" => {
            (!object.contains_key("max_bytes")
                || params["max_bytes"]
                    .as_u64()
                    .is_some_and(|v| v > 0 && v <= MAX_RESULT_BYTES as u64))
                && (!object.contains_key("output")
                    || (nonempty(&params["output"], 4096)
                        && !params["output"].as_str().unwrap_or("").contains('\0')))
        }
        "browser.wait" => params["condition"].as_object().is_some_and(|c| {
            c.len() == 1
                && (nonempty(&params["condition"]["text"], 4096)
                    || params["condition"]["load"].as_bool() == Some(true))
        }),
        "browser.dialog" => {
            params["accept"].is_boolean()
                && (!object.contains_key("prompt_text")
                    || params["prompt_text"]
                        .as_str()
                        .is_some_and(|s| s.len() <= 4096))
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid())
    }
}

struct WakeState {
    deadline: Option<Instant>,
    stopped: bool,
}
struct DeadlineWake {
    shared: Arc<(Mutex<WakeState>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl DeadlineWake {
    fn new(tx: Sender<AppEvent>) -> Result<Self, String> {
        let shared = Arc::new((
            Mutex::new(WakeState {
                deadline: None,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("browser-deadline".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let (lock, changed) = &*state;
                let mut guard = lock.lock().expect("browser deadline lock");
                loop {
                    if guard.stopped {
                        break;
                    }
                    match guard.deadline {
                        None => guard = changed.wait(guard).expect("browser deadline wait"),
                        Some(at) if at > Instant::now() => {
                            guard = changed
                                .wait_timeout(guard, at.saturating_duration_since(Instant::now()))
                                .expect("browser deadline wait")
                                .0;
                        }
                        Some(_) => {
                            guard.deadline = None;
                            if tx.send(AppEvent::BrowserControlTimeout).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|_| "browser deadline unavailable".to_string())?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    fn arm(&self, deadline: Option<Instant>) {
        self.shared
            .0
            .lock()
            .expect("browser deadline lock")
            .deadline = deadline;
        self.shared.1.notify_one();
    }
}
impl Drop for DeadlineWake {
    fn drop(&mut self) {
        self.shared.0.lock().expect("browser deadline lock").stopped = true;
        self.shared.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};

    fn target() -> Target {
        Target {
            generation: "owner-generation".into(),
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            pane_id: "1".into(),
            terminal_id: None,
        }
    }
    fn request(registry: &mut BrowserRegistry, method: &str, params: Value) -> Receiver<String> {
        let (reply, rx) = channel();
        registry.handle_api_request(ApiRequest {
            id: "api-request".into(),
            method: method.into(),
            params,
            reply,
        });
        rx
    }
    fn response(rx: &Receiver<String>) -> Value {
        serde_json::from_str(&rx.try_recv().unwrap()).unwrap()
    }
    fn fixture() -> (BrowserRegistry, String) {
        let mut r = BrowserRegistry::default();
        let (id, launch) = r
            .register(7, target(), "https://example.com".into())
            .unwrap();
        assert_eq!(r.get(&id).unwrap()["state"], "preparing");
        assert!(r.complete(7, launch["request_id"].as_str().unwrap(), Ok(json!({}))));
        (r, id)
    }
    fn acquire(r: &mut BrowserRegistry, id: &str) -> String {
        let rx = request(r, "browser.control.acquire", json!({"browser":id}));
        assert!(rx.try_recv().is_err(), "queue admission is not completion");
        let command = r.take_commands().pop().unwrap().1;
        assert_eq!(command["params"]["ttl_s"], 300);
        assert!(command["params"].get("lease").is_none());
        assert!(r.complete(7, command["request_id"].as_str().unwrap(), Ok(json!({}))));
        let value = response(&rx);
        assert_eq!(value["result"]["state"], "completed");
        value["result"]["lease"].as_str().unwrap().into()
    }

    #[test]
    fn browser_control_wrong_display_target_and_generation_cannot_consume_completion() {
        let mut r = BrowserRegistry::default();
        let (id, launch) = r
            .register(7, target(), super::super::BLANK_URL.into())
            .unwrap();
        let request_id = launch["request_id"].as_str().unwrap();
        assert!(!r.complete(8, request_id, Ok(json!({}))));
        let mut wrong = target();
        wrong.generation = "new-owner".into();
        assert!(!r.complete_bound(7, request_id, &wrong, 0, Ok(json!({}))));
        assert!(!r.complete_bound(7, request_id, &target(), 1, Ok(json!({}))));
        assert!(r.complete_bound(7, request_id, &target(), 0, Ok(json!({}))));
        assert_eq!(r.get(&id).unwrap()["state"], "ready");
        assert!(!r.complete(7, request_id, Ok(json!({}))));
    }

    #[test]
    fn browser_control_read_methods_do_not_grant_or_disclose_control() {
        let (mut r, id) = fixture();
        for method in ["browser.get", "browser.control.status"] {
            let value = response(&request(&mut r, method, json!({"browser":id})));
            assert_eq!(value["result"]["control"]["state"], "uncontrolled");
            assert!(r.browsers[&id].lease.is_none());
        }
        let lease = acquire(&mut r, &id);
        for (method, params) in [
            ("browser.list", json!({})),
            ("browser.get", json!({"browser":id})),
            ("browser.control.status", json!({"browser":id})),
        ] {
            let value = response(&request(&mut r, method, params));
            assert!(!value.to_string().contains(&lease));
        }
        assert!(r.take_commands().is_empty());
        let denied = response(&request(
            &mut r,
            "browser.evaluate",
            json!({"browser":id,"lease":"wrong","expression":"1"}),
        ));
        assert_eq!(denied["error"]["code"], "control_required");
        let busy = response(&request(
            &mut r,
            "browser.control.acquire",
            json!({"browser":id}),
        ));
        assert_eq!(busy["error"]["code"], "control_busy");
    }

    #[test]
    fn browser_control_pending_limit_and_display_session_limit_are_bounded() {
        let (mut r, id) = fixture();
        let lease = acquire(&mut r, &id);
        let mut replies = Vec::new();
        for _ in 0..MAX_PENDING {
            replies.push(request(
                &mut r,
                "browser.snapshot",
                json!({"browser":id,"lease":lease}),
            ));
        }
        assert_eq!(r.pending.len(), MAX_PENDING);
        let rejected = response(&request(
            &mut r,
            "browser.snapshot",
            json!({"browser":id,"lease":lease}),
        ));
        assert_eq!(rejected["error"]["code"], "limit_exceeded");
        assert_eq!(r.take_commands().len(), MAX_PENDING);
        r.detach(7);
        for rx in replies {
            assert_eq!(response(&rx)["error"]["code"], "display_detached");
        }
        for _ in 0..MAX_PER_DISPLAY {
            r.register(7, target(), "about:blank".into()).unwrap();
        }
        assert_eq!(
            r.register(7, target(), "about:blank".into()).unwrap_err(),
            "limit_exceeded"
        );
    }

    #[test]
    fn browser_control_release_cancels_a_full_queue_and_requires_new_ownership() {
        let (mut r, id) = fixture();
        let lease = acquire(&mut r, &id);
        let mut pending = Vec::new();
        for _ in 0..MAX_PENDING {
            pending.push(request(
                &mut r,
                "browser.snapshot",
                json!({"browser":id,"lease":lease}),
            ));
        }
        let rx = request(
            &mut r,
            "browser.control.release",
            json!({"browser":id,"lease":lease}),
        );
        for reply in pending {
            assert_eq!(response(&reply)["error"]["code"], "control_released");
        }
        assert!(rx.try_recv().is_err());
        let commands = r.take_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].1["method"], "browser.control.release");
        assert!(r.complete(
            7,
            commands[0].1["request_id"].as_str().unwrap(),
            Ok(json!({}))
        ));
        assert_eq!(response(&rx)["result"]["state"], "completed");
        let replacement = acquire(&mut r, &id);
        assert_ne!(lease, replacement);
        let denied = response(&request(
            &mut r,
            "browser.reload",
            json!({"browser":id,"lease":lease}),
        ));
        assert_eq!(denied["error"]["code"], "control_required");
        let (other, launch) = r.register(8, target(), "about:blank".into()).unwrap();
        assert!(r.complete(8, launch["request_id"].as_str().unwrap(), Ok(json!({}))));
        let denied = response(&request(
            &mut r,
            "browser.reload",
            json!({"browser":other,"lease":replacement}),
        ));
        assert_eq!(denied["error"]["code"], "control_required");
    }

    #[test]
    fn browser_control_detach_and_route_revocation_discard_late_results() {
        for detach in [true, false] {
            let (mut r, id) = fixture();
            let lease = acquire(&mut r, &id);
            let rx = request(
                &mut r,
                "browser.reload",
                json!({"browser":id,"lease":lease}),
            );
            let command = r.take_commands().pop().unwrap().1;
            if detach {
                r.detach(7);
            } else {
                r.revoke(&id);
            }
            assert!(response(&rx).get("error").is_some());
            assert!(!r.complete(7, command["request_id"].as_str().unwrap(), Ok(json!({}))));
            if detach {
                assert!(r.get(&id).is_none());
                assert!(r.take_commands().is_empty());
            } else {
                assert_eq!(r.get(&id).unwrap()["state"], "revoked");
            }
        }
    }

    #[test]
    fn browser_control_human_takeover_revokes_without_automatic_reacquisition() {
        let (mut r, id) = fixture();
        let lease = acquire(&mut r, &id);
        assert!(
            !r.display_takeover(7, &id, Some(0)),
            "late revoke from an earlier controller"
        );
        let rx = request(
            &mut r,
            "browser.navigate",
            json!({"browser":id,"lease":lease,"url":"https://other.test"}),
        );
        assert!(!r.human_takeover(8, &id));
        assert!(r.human_takeover(7, &id));
        assert_eq!(response(&rx)["error"]["code"], "control_revoked");
        assert_eq!(r.get(&id).unwrap()["control"]["state"], "uncontrolled");
        let commands = r.take_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].1["method"], "browser.control.release");
        let denied = response(&request(
            &mut r,
            "browser.reload",
            json!({"browser":id,"lease":lease}),
        ));
        assert_eq!(denied["error"]["code"], "control_required");
        assert!(r.take_commands().is_empty());
    }

    #[test]
    fn browser_control_expiry_and_command_deadline_revoke_pending_replies() {
        for lease_expiry in [true, false] {
            let (mut r, id) = fixture();
            let lease = acquire(&mut r, &id);
            let rx = request(
                &mut r,
                "browser.wait",
                json!({"browser":id,"lease":lease,"condition":{"load":true}}),
            );
            let command = r.take_commands().pop().unwrap().1;
            let now = Instant::now();
            if lease_expiry {
                r.browsers
                    .get_mut(&id)
                    .unwrap()
                    .lease
                    .as_mut()
                    .unwrap()
                    .deadline = now;
            } else {
                r.pending.values_mut().next().unwrap().deadline = now;
            }
            r.expire_at(now);
            assert_eq!(
                response(&rx)["error"]["code"],
                if lease_expiry {
                    "lease_expired"
                } else {
                    "timeout"
                }
            );
            assert!(!r.complete(7, command["request_id"].as_str().unwrap(), Ok(json!({}))));
            assert!(r.browsers[&id].lease.is_none());
        }
    }

    #[test]
    fn browser_control_deadline_wakes_once_without_polling() {
        let (tx, events) = channel();
        let mut r = BrowserRegistry::new(tx);
        let (_, launch) = r.register(7, target(), "about:blank".into()).unwrap();
        r.pending
            .get_mut(launch["request_id"].as_str().unwrap())
            .unwrap()
            .deadline = Instant::now();
        r.arm();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            AppEvent::BrowserControlTimeout
        ));
        r.expire();
        assert!(events.try_recv().is_err());
        assert!(r.pending.is_empty());
    }

    #[test]
    fn browser_control_result_budgets_and_network_errors_are_not_leaked() {
        let (mut r, id) = fixture();
        let lease = acquire(&mut r, &id);
        for result in [
            Ok(json!("x".repeat(MAX_RESULT_BYTES))),
            Err("proxy://user:secret@host /native/socket".into()),
        ] {
            let rx = request(
                &mut r,
                "browser.screenshot",
                json!({"browser":id,"lease":lease}),
            );
            let command = r.take_commands().pop().unwrap().1;
            assert!(r.complete(7, command["request_id"].as_str().unwrap(), result));
            let value = response(&rx);
            assert!(value.get("error").is_some());
            assert!(!value.to_string().contains("secret"));
            assert!(!value.to_string().contains("socket"));
        }
        assert!(validate_params(
            "browser.screenshot",
            &json!({"browser":id,"lease":lease,"max_bytes":MAX_RESULT_BYTES+1})
        )
        .is_err());
    }

    #[test]
    fn browser_control_canonical_display_revocations_fail_closed() {
        for code in [
            "route_changed",
            "browser_closed",
            "timeout",
            "control_revoked",
        ] {
            let (mut r, id) = fixture();
            let lease = acquire(&mut r, &id);
            let rx = request(
                &mut r,
                "browser.snapshot",
                json!({"browser":id,"lease":lease}),
            );
            let command = r.take_commands().pop().unwrap().1;
            assert!(r.complete(7, command["request_id"].as_str().unwrap(), Err(code.into())));
            assert_eq!(response(&rx)["error"]["code"], code);
            assert!(r.browsers[&id].lease.is_none());
            assert!(r.pending.is_empty());
        }
        assert_eq!(display_error("stale_reference"), "stale_reference");
        assert_eq!(display_error("timeout: /private/socket"), "display_failed");
    }

    #[test]
    fn browser_control_public_fixtures_match_runtime_validation() {
        let mut covered = std::collections::BTreeSet::new();
        for (source, valid) in [
            (
                include_str!("../../protocol/uhp/v1/fixtures/valid/requests.jsonl"),
                true,
            ),
            (
                include_str!("../../protocol/uhp/v1/fixtures/invalid/requests.jsonl"),
                false,
            ),
        ] {
            for line in source.lines() {
                let value: Value = serde_json::from_str(line).unwrap();
                let Some(method) = value["method"].as_str().filter(|m| METHODS.contains(m)) else {
                    continue;
                };
                assert_eq!(
                    validate_params(method, &value["params"]).is_ok(),
                    valid,
                    "{line}"
                );
                if valid {
                    covered.insert(method.to_owned());
                }
            }
        }
        assert_eq!(covered, METHODS.iter().map(|m| (*m).to_owned()).collect());
    }

    #[test]
    fn browser_control_closed_shapes_reject_unsafe_or_unbounded_parameters() {
        for (method, params) in [
            ("browser.get", json!({"browser":"b","lease":"secret"})),
            ("browser.control.acquire", json!({"browser":"b","ttl_s":0})),
            (
                "browser.control.acquire",
                json!({"browser":"b","ttl_s":3601}),
            ),
            (
                "browser.control.acquire",
                json!({"browser":"b","force":true}),
            ),
            (
                "browser.navigate",
                json!({"browser":"b","lease":"l","url":"file:///private/test"}),
            ),
            (
                "browser.act",
                json!({"browser":"b","lease":"l","action":{"kind":"click","node":"n"}}),
            ),
            (
                "browser.act",
                json!({"browser":"b","lease":"l","action":{"kind":"fill","node":"n","document_revision":"1","text":"ok","command":"sh"}}),
            ),
            (
                "browser.evaluate",
                json!({"browser":"b","lease":"l","expression":"x".repeat(32769)}),
            ),
            (
                "browser.wait",
                json!({"browser":"b","lease":"l","condition":{"load":false}}),
            ),
        ] {
            assert!(validate_params(method, &params).is_err(), "{method}");
        }
    }
}
