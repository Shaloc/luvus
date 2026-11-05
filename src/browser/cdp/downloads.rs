//! A CDP controller grants a directory on the owner. The guest only receives a
//! random permit; neither its URL nor its suggested name can select a directory.

use super::{Failure, GatewayResult};
use crate::browser::files::WorkspaceFiles;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

const LIMIT: usize = 64 * 1024 * 1024;
const INVALID: Failure = Failure::new("invalid_params", "invalid download request");
const DENIED: Failure = Failure::new("file_denied", "owner destination unavailable");

struct Permit {
    id: String,
    directory: WorkspaceFiles,
    by_guid: bool,
    events: bool,
    live: AtomicBool,
    waiting: Mutex<std::sync::Weak<mpsc::SyncSender<Option<Vec<u8>>>>>,
}

#[derive(Default)]
pub(super) struct Policy(Mutex<Option<Arc<Permit>>>);

impl Policy {
    pub(super) fn configure(
        &self,
        root: Option<&WorkspaceFiles>,
        message: &mut Value,
    ) -> GatewayResult<()> {
        let params = message["params"].as_object_mut().ok_or(INVALID)?;
        if params.keys().any(|k| {
            !matches!(
                k.as_str(),
                "behavior" | "eventsEnabled" | "browserContextId" | "downloadPath"
            )
        }) {
            return Err(INVALID);
        }
        let events = match params.get("eventsEnabled") {
            None => false,
            Some(v) => v.as_bool().ok_or(INVALID)?,
        };
        let permit = match params.get("behavior").and_then(Value::as_str) {
            Some(mode @ ("allow" | "allowAndName")) => {
                let path = params
                    .get("downloadPath")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 4096)
                    .ok_or(INVALID)?;
                let directory = root
                    .ok_or(DENIED)?
                    .subdirectory(Path::new(path))
                    .map_err(|_| DENIED)?;
                Some(Arc::new(Permit {
                    id: crate::terminal::backend::random_id().map_err(|_| DENIED)?,
                    directory,
                    by_guid: mode == "allowAndName",
                    events,
                    live: AtomicBool::new(true),
                    waiting: Mutex::default(),
                }))
            }
            Some("default" | "deny") if !params.contains_key("downloadPath") => None,
            _ => return Err(INVALID),
        };
        params.remove("downloadPath");
        params.insert("permit".into(), json!(permit.as_ref().map(|p| &p.id)));
        message["method"] = json!("Luvus.setDownloadBehavior");
        let mut current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = current.take() {
            old.live.store(false, Ordering::Release);
            // The producer may be idle waiting for Chromium to finish. Wake
            // the pinned file writer without keeping its channel alive.
            if let Some(writer) = old
                .waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .upgrade()
            {
                let _ = writer.try_send(None);
            }
        }
        *current = permit;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    permit: String,
    guid: String,
    body: Body,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Body {
    Begin {
        name: String,
        size: usize,
        url: String,
        frame: String,
        session: Option<String>,
    },
    Data {
        offset: usize,
        data: String,
    },
    End,
    Cancel,
}

struct Writing {
    permit: String,
    guid: String,
    offset: usize,
    size: usize,
    tx: Arc<mpsc::SyncSender<Option<Vec<u8>>>>,
}

pub(super) struct Receiver {
    active: Option<Writing>,
    workers: Vec<JoinHandle<()>>,
    alive: Arc<AtomicBool>,
}

impl Default for Receiver {
    fn default() -> Self {
        Self {
            active: None,
            workers: vec![],
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.active.take(); // Wake a worker awaiting an explicit commit.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn event(method: &str, params: Value, session: &Option<String>) -> Value {
    let mut value = json!({"method":method,"params":params});
    if let Some(session) = session {
        value["sessionId"] = json!(session);
    }
    value
}

impl Receiver {
    pub(super) fn receive(
        &mut self,
        policy: &Policy,
        text: &str,
        emit: impl Fn(Value) + Send + 'static,
    ) -> GatewayResult<()> {
        if text.len() > 128 * 1024 {
            return Err(INVALID);
        }
        #[derive(Deserialize)]
        struct Envelope {
            download: Record,
        }
        let Envelope {
            download: Record { permit, guid, body },
        } = serde_json::from_str(text)
            .map_err(|_| Failure::new("invalid_stream", "invalid native download record"))?;
        let current = policy.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(current) = current.filter(|p| p.id == permit) else {
            if self.active.as_ref().is_some_and(|a| a.permit == permit) {
                self.active.take();
            }
            return Ok(()); // Late data from an explicitly replaced policy is inert.
        };
        if !super::opaque_id(&guid) {
            return Err(INVALID);
        }
        if let Body::Begin {
            name,
            size,
            url,
            frame,
            session,
        } = body
        {
            self.workers.retain(|w| !w.is_finished());
            if self.active.as_ref().is_some_and(|a| a.permit != permit) {
                self.active.take();
            }
            if self.active.is_some()
                || self.workers.len() >= 2
                || size > LIMIT
                || name.is_empty()
                || name.len() > 255
                || matches!(name.as_str(), "." | "..")
                || name.contains(['/', '\\', ':'])
                || name.chars().any(char::is_control)
                || url.len() > 8192
                || !crate::browser::valid_url(&url)
                || frame.len() > 256
                || session.as_ref().is_some_and(|s| s.len() > 256)
            {
                return Err(INVALID);
            }
            if current.events {
                emit(event(
                    "Browser.downloadWillBegin",
                    json!({"guid":guid,"frameId":frame,"url":url,"suggestedFilename":name}),
                    &session,
                ));
            }
            let file = if current.by_guid { guid.clone() } else { name };
            let (tx, rx) = mpsc::sync_channel(8);
            let tx = Arc::new(tx);
            *current.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&tx);
            let worker_guid = guid.clone();
            let live = self.alive.clone();
            let worker = std::thread::Builder::new().name("luvus-browser-download".into()).spawn(move || {
                let cancelled = || !live.load(Ordering::Acquire) || !current.live.load(Ordering::Acquire);
                let ok = current.directory.write_download(Path::new(&file), size, rx, cancelled).is_ok();
                if current.events && live.load(Ordering::Acquire) {
                    let mut data = json!({"guid":worker_guid,"totalBytes":size,"receivedBytes":if ok {size} else {0},
                        "state":if ok {"completed"} else {"canceled"}});
                    if ok { data["filePath"] = json!(current.directory.destination(&file)); }
                    emit(event("Browser.downloadProgress", data, &session));
                }
            }).map_err(|_| DENIED)?;
            self.workers.push(worker);
            self.active = Some(Writing {
                permit,
                guid,
                offset: 0,
                size,
                tx,
            });
            return Ok(());
        }
        let Some(writing) = self
            .active
            .as_mut()
            .filter(|a| a.guid == guid && a.permit == permit)
        else {
            return Err(INVALID);
        };
        match body {
            Body::Data { offset, data } => {
                if writing.offset != offset {
                    return Err(INVALID);
                }
                let bytes = crate::terminal::clipboard::decode_base64(&data, 64 * 1024)
                    .map_err(|_| INVALID)?;
                if bytes.len() > writing.size - writing.offset {
                    return Err(INVALID);
                }
                writing.offset += bytes.len();
                match writing.tx.try_send(Some(bytes)) {
                    Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {} // Writer reports filesystem failure.
                    Err(mpsc::TrySendError::Full(_)) => {
                        return Err(Failure::new("limit_exceeded", "download writer is slow"))
                    }
                }
            }
            Body::End => {
                if writing.offset != writing.size {
                    return Err(INVALID);
                }
                if let Err(mpsc::TrySendError::Full(_)) = writing.tx.try_send(None) {
                    return Err(DENIED);
                }
                self.active.take();
            }
            Body::Cancel => {
                self.active.take();
            }
            Body::Begin { .. } => unreachable!(),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cdp_download_records_accept_only_the_fields_for_their_phase() {
        let parse = |body: Value| {
            serde_json::from_value::<Record>(json!({"permit":"scope","guid":"file","body":body}))
        };
        assert!(parse(json!({"kind":"begin","name":"a.bin","size":1,"url":"https://example.test","frame":"f","session":"s"})).is_ok());
        assert!(parse(json!({"kind":"data","offset":0,"data":"YQ=="})).is_ok());
        assert!(
            parse(json!({"kind":"data","offset":0,"data":"YQ==","path":"/unauthorized"})).is_err()
        );
    }
}
