use std::collections::HashMap;
use std::sync::mpsc;
use std::thread::JoinHandle;

use serde::Deserialize;
use serde_json::{json, Value};

use super::protocol::{Response, Target};
use super::{emit, Output};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    request_id: String,
    browser_id: String,
    target: Target,
    url: String,
    lease_generation: String,
    launch: crate::browser::LaunchRequest,
}

struct Entry {
    target: Target,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
pub(super) struct Sessions(HashMap<String, Entry>);

impl Sessions {
    pub fn open(
        &mut self,
        data: Value,
        route: &Result<Option<String>, String>,
        output: Output,
    ) -> Result<(), String> {
        let mut request: Launch =
            serde_json::from_value(data).map_err(|_| "invalid browser launch")?;
        if self.0.len() >= 8
            || self.0.contains_key(&request.browser_id)
            || !crate::terminal::backend::valid_id(&request.browser_id)
            || !crate::terminal::backend::valid_id(&request.request_id)
            || request.url != request.launch.request.url
            || !request.launch.request.proxied
            || request.launch.proxy.is_none()
        {
            return Err("invalid browser launch identity or capacity".into());
        }
        match route {
            Ok(Some(host)) => request.launch.request.hosts.push(host.clone()),
            Ok(None) => {}
            Err(error) => return Err(error.clone()),
        }
        request.launch.request.validate()?;
        let (stop, cancelled) = mpsc::channel();
        let worker_stop = stop.clone();
        let id = request.browser_id.clone();
        let target = request.target.clone();
        let worker = std::thread::Builder::new().name("app-browser-network".into()).spawn(move || {
            match crate::browser::NetworkLease::open(&request.launch, worker_stop, cancelled) {
                Ok(mut lease) => {
                    let response = Response::Effect { kind: "browser_open".into(), data: json!({
                        "browser_id":request.browser_id,"request_id":request.request_id,
                        "target":request.target,"lease_generation":request.lease_generation,
                        "url":request.url,"proxy_port":lease.port(),
                    }) };
                    if emit(&output, &response).is_err() { return; }
                    if let Some(closed) = lease.take_closed() { let _ = closed.recv(); }
                    drop(lease);
                    let _ = emit(&output, &Response::Effect { kind: "browser_closed".into(), data: json!({"browser_id":request.browser_id}) });
                }
                Err(_) => {
                    let _ = emit(&output, &Response::Effect { kind: "browser_error".into(), data: json!({
                        "browser_id":request.browser_id,"request_id":request.request_id,
                        "target":request.target,"lease_generation":request.lease_generation,
                        "code":"display_failed",
                    }) });
                }
            }
        }).map_err(|_| "cannot start browser network worker")?;
        self.0.insert(
            id,
            Entry {
                target,
                stop,
                worker: Some(worker),
            },
        );
        Ok(())
    }

    pub fn close(&mut self, id: &str) {
        self.0.remove(id);
    }

    pub fn retain_snapshot(&mut self, snapshot: &Value) {
        self.0.retain(|_, entry| {
            snapshot["server_generation"].as_str() == Some(entry.target.generation.as_str())
                && snapshot["workspaces"].as_array().is_some_and(|workspaces| {
                    workspaces.iter().any(|ws| {
                        ws["id"].as_str() == Some(entry.target.workspace_id.as_str())
                            && ws["tabs"].as_array().is_some_and(|tabs| {
                                tabs.iter().any(|tab| {
                                    tab["id"].as_str() == Some(entry.target.tab_id.as_str())
                                        && tab["panes"].as_array().is_some_and(|panes| {
                                            panes.iter().any(|pane| {
                                                pane["pane_id"].as_str()
                                                    == Some(entry.target.pane_id.as_str())
                                            })
                                        })
                                })
                            })
                    })
                })
        });
    }
}
