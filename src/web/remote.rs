//! Browser projection of the topology already maintained by App's SSH owners.
//! No discovery, polling, credentials, or alternate SSH policy lives here.
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

use super::uhp::UhpError;
use crate::session::remote::RemoteSession;

#[derive(Clone, Debug)]
pub(super) struct Route {
    target: RemoteSession,
    generation: String,
    pane: String,
    terminal: String,
    browser_pane: String,
    browser_terminal: String,
    browser_generation: String,
}

/// Namespace owner-local IDs without altering the native snapshot contract.
/// Only projected workspaces are admitted; nested federation is not followed.
pub(super) fn project(snapshot: &mut Value) -> HashMap<String, Route> {
    let mut routes = HashMap::new();
    let browser_generation = snapshot["server_generation"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let Some(workspaces) = snapshot["workspaces"].as_array_mut() else {
        return routes;
    };
    for workspace in workspaces {
        let Some(remote) = workspace.as_object_mut().and_then(|w| w.remove("remote")) else {
            continue;
        };
        let Some(target) = remote["host"]
            .as_str()
            .zip(remote["session"].as_str())
            .and_then(|(host, session)| RemoteSession::new(host, session).ok())
        else {
            continue;
        };
        let Some(generation) = remote["server_generation"].as_str() else {
            continue;
        };
        let Some(mut tabs) = remote["tabs"].as_array().cloned() else {
            continue;
        };
        let namespace = format!(
            "{:x}",
            Sha256::digest(
                json!([
                    target.host,
                    target.session,
                    remote["workspace_id"],
                    generation
                ])
                .to_string()
                .as_bytes()
            )
        );
        let connected = remote["connected"].as_bool() == Some(true);
        for tab in &mut tabs {
            let Some(panes) = tab["panes"].as_array_mut() else {
                continue;
            };
            for pane in panes {
                let Some(owner_pane) = pane["pane_id"].as_str().map(str::to_string) else {
                    continue;
                };
                let browser_pane = format!("remote:{namespace}:{owner_pane}");
                pane["display_pane_id"] = json!(owner_pane);
                pane["pane_id"] = json!(browser_pane);
                // Native session IDs are not needed by the browser.
                if let Some(object) = pane.as_object_mut() {
                    object.remove("agent_session");
                }
                if connected {
                    if let Some(terminal) = pane["terminal_id"].as_str().map(str::to_string) {
                        let browser_terminal = format!("remote:{namespace}:{terminal}");
                        routes.insert(
                            browser_terminal.clone(),
                            Route {
                                target: target.clone(),
                                generation: generation.into(),
                                pane: owner_pane,
                                terminal,
                                browser_pane,
                                browser_terminal: browser_terminal.clone(),
                                browser_generation: browser_generation.clone(),
                            },
                        );
                        pane["terminal_id"] = json!(browser_terminal);
                    }
                } else {
                    pane["terminal_id"] = Value::Null;
                    pane["agent_status"] = Value::Null;
                }
            }
        }
        workspace["tabs"] = json!(tabs);
        workspace["name"] = json!(format!(
            "{} · {}/{}{}",
            workspace["name"].as_str().unwrap_or("Workspace"),
            target.host,
            target.session,
            if connected { "" } else { " (offline)" }
        ));
    }
    routes
}

pub(super) fn resolve(snapshot: &mut Value, params: &Value) -> Result<Route, UhpError> {
    if params["server_generation"] != snapshot["server_generation"] {
        return Err(UhpError::coded("stale_server", "Local session changed"));
    }
    let routes = project(snapshot);
    let route = params["terminal_id"]
        .as_str()
        .and_then(|id| routes.get(id))
        .filter(|route| params["pane_id"].as_str() == Some(&route.browser_pane))
        .ok_or_else(|| {
            UhpError::coded("stale_terminal", "Remote terminal is no longer available")
        })?;
    Ok(route.clone())
}

/// One bounded browser stream uses the existing existing-only SSH control
/// bridge. Dropping both halves kills only the SSH child, never the owner.
pub(super) enum Stream {
    Local(tokio::net::TcpStream),
    Remote {
        child: Arc<Mutex<tokio::process::Child>>,
        route: Box<Route>,
        input: tokio::process::ChildStdin,
        output: tokio::process::ChildStdout,
    },
}

impl Stream {
    pub(super) fn child(&self) -> Option<Weak<Mutex<tokio::process::Child>>> {
        match self {
            Self::Remote { child, .. } => Some(Arc::downgrade(child)),
            Self::Local(_) => None,
        }
    }

    pub(super) fn route(&self) -> Option<Route> {
        match self {
            Self::Remote { route, .. } => Some((**route).clone()),
            Self::Local(_) => None,
        }
    }
}

impl Route {
    pub(super) fn project_frame(&self, frame: &mut Value) {
        for key in ["result", "data"] {
            if let Some(value) = frame.get_mut(key).and_then(Value::as_object_mut) {
                for (key, replacement) in [
                    ("server_generation", &self.browser_generation),
                    ("pane_id", &self.browser_pane),
                    ("terminal_id", &self.browser_terminal),
                ] {
                    if value.contains_key(key) {
                        value.insert(key.into(), json!(replacement));
                    }
                }
            }
        }
    }

    pub(super) async fn open(
        self,
        method: &str,
        mut params: Value,
        id: &str,
    ) -> Result<Stream, UhpError> {
        let host = self.target.host.clone();
        let location = tokio::task::spawn_blocking(move || {
            crate::session::remote::verify_remote_version(&host)
        })
        .await
        .map_err(|error| UhpError::unavailable(error.to_string()))?
        .map_err(UhpError::unavailable)?;
        let command =
            crate::session::remote::bridge_command(&self.target, "remote-control-bridge", location);
        let mut child = tokio::process::Command::from(command)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| UhpError::unavailable(error.to_string()))?;
        let mut input = child.stdin.take().expect("piped remote input");
        let output = child.stdout.take().expect("piped remote output");
        params["server_generation"] = json!(self.generation);
        params["pane_id"] = json!(self.pane);
        params["terminal_id"] = json!(self.terminal);
        let frame = json!({"id":id, "method":method, "params":params});
        tokio::time::timeout(
            std::time::Duration::from_secs(12),
            input.write_all(format!("{frame}\n").as_bytes()),
        )
        .await
        .map_err(|_| UhpError::unavailable("remote request timed out"))?
        .map_err(|error| UhpError::unavailable(error.to_string()))?;
        Ok(Stream::Remote {
            child: Arc::new(Mutex::new(child)),
            route: Box::new(self),
            input,
            output,
        })
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Local(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Remote { output, .. } => Pin::new(output).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Local(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Remote { input, .. } => Pin::new(input).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Local(stream) => Pin::new(stream).poll_flush(cx),
            Self::Remote { input, .. } => Pin::new(input).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Local(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Remote { input, .. } => Pin::new(input).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        let pane = json!({"kind":"terminal","pane_id":"1","terminal_id":"terminal-1","is_agent":true,"agent":"codex","agent_status":"blocked","agent_session_title":"Review changes"});
        let tabs = json!([{"index":1,"panes":[pane]}]);
        let remote = json!({"name":"Project","remote":{"host":"dev-63","session":"api","workspace_id":"workspace-a","server_generation":"owner-a","connected":true,"tabs":tabs}});
        let mut other = remote.clone();
        other["remote"]["session"] = json!("second");
        json!({"server_generation":"local-a","workspaces":[{"name":"Local","tabs":tabs}, remote, other]})
    }

    #[test]
    fn remote_web_projection_preserves_local_and_separates_owners() {
        let mut snapshot = fixture();
        let local = snapshot["workspaces"][0].clone();
        let routes = project(&mut snapshot);
        assert_eq!(snapshot["workspaces"][0], local);
        assert_eq!(routes.len(), 2);
        let a = &snapshot["workspaces"][1]["tabs"][0]["panes"][0];
        let b = &snapshot["workspaces"][2]["tabs"][0]["panes"][0];
        assert_ne!(a["pane_id"], b["pane_id"]);
        assert_ne!(a["terminal_id"], b["terminal_id"]);
        assert_eq!(a["agent_status"], "blocked");
        assert_eq!(a["agent_session_title"], "Review changes");
        assert_eq!(a["display_pane_id"], "1");
        assert!(snapshot["workspaces"][1].get("remote").is_none());
        let route = routes.get(a["terminal_id"].as_str().unwrap()).unwrap();
        let mut frame = json!({"event":"terminal.frame","data":{"server_generation":"owner-a","terminal_id":"terminal-1","pane_id":"1","text":"owner output"}});
        route.project_frame(&mut frame);
        assert_eq!(frame["data"]["terminal_id"], a["terminal_id"]);
        assert_eq!(frame["data"]["server_generation"], "local-a");
        assert_eq!(frame["data"]["text"], "owner output");
    }

    #[test]
    fn remote_web_routes_reject_restart_disconnect_close_and_cross_pane_replay() {
        let original = fixture();
        let mut snapshot = original.clone();
        project(&mut snapshot);
        let pane = &snapshot["workspaces"][1]["tabs"][0]["panes"][0];
        let params = json!({"server_generation":"local-a","terminal_id":pane["terminal_id"],"pane_id":pane["pane_id"]});
        assert!(resolve(&mut original.clone(), &params).is_ok());
        let mut wrong = params.clone();
        wrong["pane_id"] = json!("1");
        assert_eq!(
            resolve(&mut original.clone(), &wrong).unwrap_err().code,
            "stale_terminal"
        );
        for (key, value) in [
            ("server_generation", json!("owner-b")),
            ("connected", json!(false)),
            ("workspace_id", json!("replacement")),
            ("host", json!("other")),
        ] {
            let mut stale = original.clone();
            stale["workspaces"][1]["remote"][key] = value;
            assert_eq!(
                resolve(&mut stale, &params).unwrap_err().code,
                "stale_terminal"
            );
        }
        let mut closed = original.clone();
        closed["workspaces"].as_array_mut().unwrap().remove(1);
        assert!(resolve(&mut closed, &params).is_err());
        let mut switched = original;
        switched["server_generation"] = json!("local-b");
        assert_eq!(
            resolve(&mut switched, &params).unwrap_err().code,
            "stale_server"
        );
    }

    #[test]
    fn remote_web_offline_snapshot_retains_cards_without_live_routes() {
        let mut snapshot = fixture();
        snapshot["workspaces"][1]["remote"]["connected"] = json!(false);
        let routes = project(&mut snapshot);
        assert_eq!(routes.len(), 1);
        let pane = &snapshot["workspaces"][1]["tabs"][0]["panes"][0];
        assert!(pane["terminal_id"].is_null());
        assert!(pane["agent_status"].is_null());
        assert_eq!(pane["agent_session_title"], "Review changes");
    }
}
