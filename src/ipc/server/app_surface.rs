use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{json, Value};

use super::{ClientState, FrameSendError, RenderClientOutcome};
use crate::app::{gui_projection::HistoryViewport, App};
use crate::event::AppEvent;
use crate::ipc::app_surface::protocol::{
    Input, MouseAction, Request, Response, SelectionMode, Target, MAX_OBJECT_BYTES, MAX_PENDING,
};
use crate::ipc::protocol::ServerMessage;
mod registered;
pub(super) use registered::{owner_request, remote_response};

#[derive(Default)]
pub(super) struct State {
    registered: HashMap<String, crate::app::gui_projection::registered::Route>,
    view_version: Option<(u64, u64, u64, &'static str)>,
    sequence: Option<u64>,
    terminals: HashMap<String, (String, u16, u16)>,
    offsets: HashMap<String, HistoryViewport>,
    viewports: HashMap<String, ViewportIntent>,
    selection: Option<(Target, String)>,
    mouse_buttons: HashMap<u8, (Target, String)>,
    pending: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl Drop for State {
    fn drop(&mut self) {
        for token in self.pending.lock().expect("App requests").values() {
            token.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone)]
struct ViewportIntent {
    target: Target,
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
}

/// Called only for the elected display, before rendering, focus or real input.
/// A passive ResizeObserver retains intent but cannot resize shared geometry.
fn apply_viewports(app: &mut App, state: &mut State, target: &Target) -> bool {
    let mut changed = false;
    state.viewports.retain(|_, intent| {
        let Ok((workspace, _, pane)) = app.validate_gui_target(&intent.target, true) else {
            return false;
        };
        if intent.target.workspace_id != target.workspace_id
            || intent.target.tab_id != target.tab_id
        {
            return true;
        }
        changed |= app.resize_terminal_viewport(pane, intent.cols, intent.rows);
        app.panes[&pane].set_cell_pixels(Some((intent.cell_width, intent.cell_height)));
        app.workspaces[workspace].cell_pixels = Some((intent.cell_width, intent.cell_height));
        true
    });
    changed
}

pub(super) fn render(
    app: &mut App,
    client: &mut ClientState,
    owns_size: bool,
    force: bool,
) -> RenderClientOutcome {
    app.refresh_registered_gui();
    registered::expire(app, client);
    let state = client.app_surface.as_mut().expect("App surface");
    if owns_size {
        let target = app.workspaces.get(app.active_ws).and_then(|workspace| {
            let tab = workspace.tabs.get(workspace.active_tab)?;
            state
                .viewports
                .values()
                .find(|intent| {
                    intent.target.workspace_id == workspace.id && intent.target.tab_id == tab.id
                })
                .map(|intent| intent.target.clone())
        });
        if let Some(target) = target {
            client.force_full |= apply_viewports(app, state, &target);
        }
    }
    let sequence = crate::ipc::api::current_sequence(&app.events);
    let view_version = (
        app.native_view_revision,
        app.bar.history_revision,
        app.registered_gui.topology,
        app.registered_gui_status(app.active_ws),
    );
    let full = force || client.force_full || client.behind;
    let mut messages = Vec::new();
    if full || state.sequence != Some(sequence) || state.view_version != Some(view_version) {
        messages.push(Response::Snapshot {
            snapshot: app.gui_snapshot(),
        });
    }
    if full || state.sequence != Some(sequence) {
        let live: HashSet<_> = app
            .panes
            .values()
            .filter_map(|pane| pane.terminal_runtime())
            .map(|runtime| runtime.terminal_id)
            .collect();
        state.offsets.retain(|id, _| live.contains(id));
        state.viewports.retain(|id, _| live.contains(id));
    }
    let mut revisions = HashMap::new();
    let mut collect =
        |frame: std::borrow::Cow<'_, crate::ipc::app_surface::protocol::TerminalFrame>| {
            let revision = (frame.revision.clone(), frame.cols, frame.rows);
            let key = frame.target.pane_id.clone();
            if full || state.terminals.get(&key) != Some(&revision) {
                messages.push(Response::Terminal {
                    frame: Box::new(frame.into_owned()),
                });
            }
            revisions.insert(key, revision);
        };
    if let Some(scene) = app.registered_scene() {
        for frame in &scene.frames {
            collect(std::borrow::Cow::Borrowed(frame));
        }
    } else {
        for frame in app.gui_terminal_frames(&mut state.offsets) {
            collect(std::borrow::Cow::Owned(frame));
        }
    }
    if messages.is_empty() {
        return RenderClientOutcome::default();
    }
    let Ok(body) = serde_json::to_string(&messages) else {
        return RenderClientOutcome {
            disconnected: true,
            ..Default::default()
        };
    };
    if body.len() > MAX_OBJECT_BYTES / 2 {
        reply(
            client,
            Response::Error {
                id: None,
                code: "resource_limit".into(),
                message: "graphical projection exceeds the attachment budget".into(),
            },
        );
        return RenderClientOutcome {
            disconnected: true,
            ..Default::default()
        };
    }
    match client.sender.try_send_frame(ServerMessage::AppFrame(body)) {
        Ok(()) => {
            let state = client.app_surface.as_mut().expect("App surface");
            state.sequence = Some(sequence);
            state.view_version = Some(view_version);
            state.terminals = revisions;
            client.force_full = false;
            client.behind = false;
            RenderClientOutcome {
                enqueued: true,
                disconnected: false,
            }
        }
        Err(FrameSendError::Full) => {
            client.behind = true;
            RenderClientOutcome::default()
        }
        Err(FrameSendError::Disconnected) => RenderClientOutcome {
            disconnected: true,
            ..Default::default()
        },
    }
}

fn reply(client: &ClientState, response: Response) {
    if let Ok(body) = serde_json::to_string(&response) {
        let _ = client.sender.send_control(ServerMessage::AppResponse(body));
    }
}

fn reject(client: &ClientState, id: &str, code: &str, message: impl Into<String>) {
    reply(
        client,
        Response::Error {
            id: Some(id.into()),
            code: code.into(),
            message: message.into(),
        },
    );
}

pub(super) fn handle(
    app: &mut App,
    client: &mut ClientState,
    client_id: u64,
    foreground: bool,
    request: Request,
) -> bool {
    if client.app_surface.is_none() {
        return false;
    }
    if let Err(message) = request.validate() {
        reject(client, request.id(), "invalid_request", message);
        return false;
    }
    if let Some(handled) = registered::forward(app, client, client_id, foreground, &request) {
        return handled;
    }
    match request {
        Request::Snapshot { id } => {
            client.force_full = true;
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"queued":true}),
                },
            );
            true
        }
        Request::Detach { .. } => {
            let _ = client.sender.send_control(ServerMessage::Detach);
            false
        }
        Request::ClipboardSucceeded { id, receipt } => {
            let accepted = foreground
                && client.surface_active()
                && client.workspace_id.is_none()
                && client.projection.is_none()
                && client.clipboard_receipt == receipt.parse::<u64>().ok();
            if accepted {
                client.clipboard_receipt = None;
                app.show_toast(app.catalog.copied);
            }
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"accepted":accepted}),
                },
            );
            accepted
        }
        Request::Action {
            id,
            generation,
            method,
            params,
        } => {
            if app
                .workspaces
                .get(app.active_ws)
                .is_some_and(|ws| ws.remote.is_some())
                && !crate::api::capabilities::is_read_only(&method)
                && !matches!(
                    method.as_str(),
                    "workspace.focus" | "browser.control.release"
                )
            {
                reject(
                    client,
                    &id,
                    "unsupported_method",
                    "action requires an owner-scoped desktop route",
                );
                return false;
            }
            if generation != app.backend_server_generation {
                reject(client, &id, "stale_target", "server generation changed");
                return false;
            }
            if !crate::api::capabilities::METHODS.contains(&method.as_str())
                || matches!(
                    method.as_str(),
                    "events.subscribe"
                        | "browser.cdp.subscribe"
                        | "terminal.backend.events.subscribe"
                        | "terminal.backend.observe"
                        | "terminal.backend.control_stream"
                )
            {
                reject(
                    client,
                    &id,
                    "unsupported_method",
                    "method is not an App action",
                );
                return false;
            }
            let mut wait_args = None;
            if method == "wait.output" {
                let timeout = crate::ipc::api::parse_timeout_s(&params);
                let pane = params["pane"].as_str().unwrap_or("");
                let needle = params["match"].as_str().unwrap_or("");
                let max = Duration::from_secs(25);
                let duration = timeout.map(|t| t.unwrap_or(max));
                if pane.is_empty()
                    || pane.len() > 128
                    || needle.is_empty()
                    || needle.len() > 4096
                    || !matches!(duration,Ok(t) if t<=max)
                {
                    reject(
                        client,
                        &id,
                        "invalid_request",
                        "provide a pane, a bounded match and a timeout of at most 25 seconds",
                    );
                    return false;
                }
                wait_args = Some((pane.to_owned(), needle.to_owned(), Some(duration.unwrap())));
            }
            let pending = client
                .app_surface
                .as_ref()
                .expect("App surface")
                .pending
                .clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            {
                let mut requests = pending.lock().expect("App requests");
                if requests.len() >= MAX_PENDING || requests.contains_key(&id) {
                    reject(
                        client,
                        &id,
                        "busy",
                        "request id is pending or the attachment is at capacity",
                    );
                    return false;
                }
                requests.insert(id.clone(), cancelled.clone());
            }
            let sender = client.sender.clone();
            let (tx, rx) = mpsc::channel::<String>();
            let request_id = id.clone();
            let timeout_token = cancelled.clone();
            std::thread::spawn(move || {
                let response = match rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(body) => match serde_json::from_str::<Value>(&body) {
                        Ok(value) => {
                            if let Some(error) = value.get("error") {
                                Response::Error {
                                    id: Some(request_id.clone()),
                                    code: error["code"].as_str().unwrap_or("request_failed").into(),
                                    message: error["message"]
                                        .as_str()
                                        .unwrap_or("request failed")
                                        .into(),
                                }
                            } else {
                                Response::Reply {
                                    id: request_id.clone(),
                                    result: value.get("result").cloned().unwrap_or(value),
                                }
                            }
                        }
                        Err(_) => Response::Error {
                            id: Some(request_id.clone()),
                            code: "invalid_response".into(),
                            message: "invalid core action response".into(),
                        },
                    },
                    Err(_) => {
                        timeout_token.store(true, Ordering::Release);
                        Response::Error {
                            id: Some(request_id.clone()),
                            code: "timeout".into(),
                            message:
                                "action response deadline elapsed; execution outcome may be unknown"
                                    .into(),
                        }
                    }
                };
                if let Ok(body) = serde_json::to_string(&response) {
                    let _ = sender.send_control(ServerMessage::AppResponse(body));
                }
                pending.lock().expect("App requests").remove(&request_id);
            });
            app.with_input_source(Some(client_id), None, |app| {
                app.handle_event(match method.as_str() {
                    "wait.output" => {
                        let (pane, needle, timeout) = wait_args.expect("validated wait");
                        AppEvent::WaitOutput {
                            id,
                            pane,
                            needle,
                            timeout,
                            reply: tx,
                            cancelled,
                        }
                    }
                    "agent.start" => AppEvent::AgentStart {
                        id,
                        params,
                        reply: tx,
                        cancelled,
                    },
                    "agent.prompt" => AppEvent::AgentPrompt {
                        id,
                        params,
                        reply: tx,
                        cancelled,
                    },
                    _ => AppEvent::Api(crate::ipc::api::ApiRequest {
                        id,
                        method,
                        params,
                        reply: tx,
                    }),
                })
            })
        }
        Request::BrowserTab { id, target, url } => {
            let Ok((wi, _, _)) = app.validate_gui_target(&target, false) else {
                reject(client, &id, "stale_target", "browser workspace changed");
                return false;
            };
            let pane = app.insert_browser_view(wi, None, url);
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"pane":pane.0.to_string()}),
                },
            );
            true
        }
        Request::MouseOptions {
            id,
            target,
            right_click_to_app,
            copy_on_select,
        } => {
            let Ok((_, _, pane)) = app.validate_gui_target(&target, true) else {
                reject(client, &id, "stale_target", "terminal changed");
                return false;
            };
            let pane = app.panes.get_mut(&pane).expect("validated terminal");
            pane.mouse_options.right_click_to_app = right_click_to_app;
            pane.mouse_options.copy_on_select = copy_on_select;
            app.session_dirty = true;
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"accepted":true}),
                },
            );
            true
        }
        Request::BrowserOpen { id, target } => {
            let (_, _, pane) = match app.validate_gui_target(&target, false) {
                Ok(target) => target,
                Err(error) => {
                    reject(client, &id, "stale_target", error);
                    return false;
                }
            };
            let Some(crate::app::ViewKind::Browser(url)) = app.views.get(&pane) else {
                reject(
                    client,
                    &id,
                    "invalid_target",
                    "target is not a browser leaf",
                );
                return false;
            };
            let existing = app
                .browser_control
                .bindings()
                .find(|(_, display, binding)| *display == client_id && *binding == &target)
                .map(|(browser, _, _)| browser.to_owned());
            if let Some(browser) = existing {
                reply(
                    client,
                    Response::Reply {
                        id,
                        result: json!({"browser_id":browser,"queued":false}),
                    },
                );
                return false;
            }
            let url = url.clone();
            match app.browser_control.register(client_id, target, url.clone()) {
                Ok((browser, mut data)) => {
                    data["launch"] = json!(crate::browser::LaunchRequest::owner(
                        crate::browser::Request {
                            url,
                            proxied: true,
                            hosts: Vec::new(),
                            origin: Some(client_id),
                        }
                    ));
                    reply(
                        client,
                        Response::Reply {
                            id,
                            result: json!({"browser_id":browser,"queued":true}),
                        },
                    );
                    reply(
                        client,
                        Response::Effect {
                            kind: "browser_launch".into(),
                            data,
                        },
                    );
                    true
                }
                Err(error) => {
                    reject(client, &id, "browser_unavailable", error);
                    false
                }
            }
        }
        Request::BrowserClosed { id, browser_id } => {
            let owned = app
                .browser_control
                .bindings()
                .any(|(browser, display, _)| browser == browser_id && display == client_id);
            if owned {
                app.browser_control.revoke(&browser_id);
            }
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"accepted":owned}),
                },
            );
            owned
        }
        Request::BrowserTakeover {
            id,
            browser_id,
            lease_generation,
        } => {
            let changed = app.browser_control.display_takeover(
                client_id,
                &browser_id,
                lease_generation.and_then(|s| s.parse().ok()),
            );
            if changed {
                reply(
                    client,
                    Response::Reply {
                        id,
                        result: json!({"accepted":true}),
                    },
                );
            } else {
                reject(
                    client,
                    &id,
                    "stale_target",
                    "browser is not owned by this display",
                );
            }
            changed
        }
        Request::BrowserCdpEvent {
            id,
            browser_id,
            target,
            lease_generation,
            chunk,
        } => {
            let accepted = app.validate_gui_target(&target, false).is_ok()
                && app.browser_control.cdp_event(
                    client_id,
                    &browser_id,
                    &target,
                    lease_generation.parse().expect("validated generation"),
                    chunk,
                );
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"accepted":accepted}),
                },
            );
            false
        }
        Request::BrowserResult {
            id,
            target,
            lease_generation,
            value,
            error,
        } => {
            let valid = app.validate_gui_target(&target, false).is_ok();
            let accepted = valid
                && app.handle_event(AppEvent::BrowserControlResult {
                    client_id,
                    request_id: id.clone(),
                    target,
                    lease_generation: lease_generation.parse().expect("validated generation"),
                    result: error.map_or(Ok(value), Err),
                });
            if accepted {
                reply(
                    client,
                    Response::Reply {
                        id,
                        result: json!({"accepted":true}),
                    },
                );
            } else {
                reject(
                    client,
                    &id,
                    "stale_target",
                    "browser completion is no longer current",
                );
            }
            accepted
        }
        Request::Focus {
            id,
            target,
            activate,
        } => {
            let (workspace, tab, pane) = match app.validate_gui_target(&target, false) {
                Ok(target) => target,
                Err(error) => {
                    reject(client, &id, "stale_target", error);
                    return false;
                }
            };
            let already_focused = workspace == app.active_ws
                && tab == app.workspaces[workspace].active_tab
                && app.workspaces[workspace].tabs[tab].layout.focus == pane;
            if foreground && activate {
                client.force_full |=
                    apply_viewports(app, client.app_surface.as_mut().unwrap(), &target);
                app.with_input_source(Some(client_id), None, |app| {
                    app.with_focus_events(|app| app.focus_location(workspace, tab, pane))
                });
            }
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"focused":foreground && (activate || already_focused)}),
                },
            );
            foreground && activate && !already_focused
        }
        Request::Widget {
            id,
            generation,
            workspace_id,
            item,
        } => {
            if !foreground || generation != app.backend_server_generation {
                reject(client, &id, "stale_target", "widget attachment changed");
                return false;
            }
            match app.desktop_widget(&workspace_id, item) {
                Ok(log) => {
                    reply(
                        client,
                        Response::Reply {
                            id,
                            result: json!({"log":log}),
                        },
                    );
                    true
                }
                Err(message) => {
                    reject(client, &id, "widget_changed", message);
                    false
                }
            }
        }
        Request::NotificationInbox {
            id,
            generation,
            mark_read,
        } => {
            if generation != app.backend_server_generation {
                reject(client, &id, "stale_target", "session changed");
                return false;
            }
            let changed = foreground && mark_read && app.bar.history.iter().any(|row| row.unread);
            if changed {
                app.bar.mark_notifications_read();
            }
            let items: Vec<_> = app.bar.history.iter().rev().map(|row| json!({
                "id": row.id.to_string(), "text": row.text, "level": row.level,
                "time": row.time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                "count": row.count
            })).collect();
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"items": items}),
                },
            );
            changed
        }
        Request::Logs {
            id,
            generation,
            client: client_log,
        } => {
            if generation != app.backend_server_generation {
                reject(client, &id, "stale_target", "server changed");
                return false;
            }
            let sender = client.sender.clone();
            let done_id = id.clone();
            if let Err(error) = app.desktop_logs(client_log, move |data| {
                let response = match data {
                    Ok(result) => Response::Reply {
                        id: done_id,
                        result,
                    },
                    Err(message) => Response::Error {
                        id: Some(done_id),
                        code: "log_unavailable".into(),
                        message,
                    },
                };
                if let Ok(body) = serde_json::to_string(&response) {
                    let _ = sender.send_control(ServerMessage::AppResponse(body));
                }
            }) {
                reject(client, &id, "unavailable", error);
            }
            false
        }
        Request::PullRequests {
            id,
            generation,
            workspace_id,
            number,
        } => {
            if generation != app.backend_server_generation {
                reject(client, &id, "stale_target", "repository generation changed");
                return false;
            }
            let sender = client.sender.clone();
            let done_id = id.clone();
            if let Err(message) = app.desktop_pull_requests(workspace_id, number, move |data| {
                let response = match data {
                    Ok(result) => Response::Reply {
                        id: done_id,
                        result,
                    },
                    Err(message) => Response::Error {
                        id: Some(done_id),
                        code: "github_unavailable".into(),
                        message,
                    },
                };
                if let Ok(body) = serde_json::to_string(&response) {
                    let _ = sender.send_control(ServerMessage::AppResponse(body));
                }
            }) {
                reject(client, &id, "unavailable", message);
            }
            false
        }
        Request::Graphic {
            id,
            target,
            sequence,
        } => {
            match app.gui_graphic(&target, app.active_ws, &sequence) {
                Ok(result) => reply(client, Response::Reply { id, result }),
                Err(code) => reject(client, &id, code, "terminal image unavailable"),
            }
            false
        }
        Request::FocusHistory {
            id,
            target,
            forward,
        } => {
            let current = app
                .validate_gui_target(&target, false)
                .is_ok_and(|(wi, ti, pane)| {
                    wi == app.active_ws
                        && app.workspaces[wi].active_tab == ti
                        && app.workspaces[wi].tabs[ti].layout.focus == pane
                });
            if !current || !foreground || app.preparing_registered_workspace() {
                reject(
                    client,
                    &id,
                    "stale_target",
                    "focus history requires the current foreground pane",
                );
                return false;
            }
            app.with_input_source(Some(client_id), None, |app| {
                app.with_focus_events(|app| {
                    app.run_cmd(if forward {
                        crate::app::Cmd::FocusForward
                    } else {
                        crate::app::Cmd::FocusBack
                    })
                })
            });
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"accepted":true}),
                },
            );
            true
        }
        Request::FileEntry {
            id,
            generation,
            workspace_id,
            root,
            path,
            operation,
        } => {
            if !foreground || generation != app.backend_server_generation {
                reject(client, &id, "stale_target", "workspace attachment changed");
                return false;
            }
            let sender = client.sender.clone();
            let done_id = id.clone();
            match app.desktop_file_entry(
                &workspace_id,
                std::path::Path::new(&root),
                path.into(),
                operation,
                move |result| {
                    let response = match result {
                        Ok(result) => Response::Reply {
                            id: done_id,
                            result,
                        },
                        Err(message) => Response::Error {
                            id: Some(done_id),
                            code: "file_failed".into(),
                            message,
                        },
                    };
                    if let Ok(body) = serde_json::to_string(&response) {
                        let _ = sender.send_control(ServerMessage::AppResponse(body));
                    }
                },
            ) {
                Ok(changed) => changed,
                Err(message) => {
                    reject(client, &id, "file_unavailable", message);
                    false
                }
            }
        }
        Request::Viewport {
            id,
            target,
            cols,
            rows,
            cell_width,
            cell_height,
        } => {
            let (_, _, _) = match app.validate_gui_target(&target, true) {
                Ok(target) => target,
                Err(error) => {
                    reject(client, &id, "stale_target", error);
                    return false;
                }
            };
            let state = client.app_surface.as_mut().unwrap();
            let terminal = target.terminal_id.clone().expect("validated terminal");
            if state.viewports.len() >= 256 && !state.viewports.contains_key(&terminal) {
                reject(
                    client,
                    &id,
                    "resource_limit",
                    "too many retained viewport intents",
                );
                return false;
            }
            state.viewports.insert(
                terminal,
                ViewportIntent {
                    target: target.clone(),
                    cols,
                    rows,
                    cell_width,
                    cell_height,
                },
            );
            let changed = foreground && apply_viewports(app, state, &target);
            client.force_full |= changed;
            reply(
                client,
                Response::Reply {
                    id,
                    result: json!({"resized":changed,"retained":true,"geometry_owner":foreground}),
                },
            );
            changed
        }
        Request::Input { id, target, input } => {
            let (workspace, tab, pane_id) = match app.validate_gui_target(&target, true) {
                Ok(target) => target,
                Err(error) => {
                    reject(client, &id, "stale_target", error);
                    return false;
                }
            };
            if workspace != app.active_ws
                || app.workspaces[workspace].active_tab != tab
                || app.preparing_registered_workspace()
            {
                reject(
                    client,
                    &id,
                    "stale_target",
                    "terminal is no longer in the selected workspace and tab",
                );
                return false;
            }
            if foreground && real_input(&input) {
                if app.workspaces[workspace].tabs[tab].layout.focus != pane_id {
                    reject(
                        client,
                        &id,
                        "stale_target",
                        "keyboard focus has moved to another terminal",
                    );
                    return false;
                }
                client.force_full |=
                    apply_viewports(app, client.app_surface.as_mut().unwrap(), &target);
                let state = client.app_surface.as_mut().unwrap();
                state.offsets.remove(target.terminal_id.as_ref().unwrap());
                state.selection = None;
            }
            if matches!(&input, Input::Paste { .. } | Input::Key { .. })
                && (!foreground || app.workspaces[workspace].tabs[tab].layout.focus != pane_id)
            {
                reject(
                    client,
                    &id,
                    "stale_target",
                    "terminal no longer owns keyboard input",
                );
                return false;
            }
            let result = match input {
                Input::Paste { text } => app
                    .panes
                    .get(&pane_id)
                    .expect("validated terminal")
                    .try_send_paste(&text)
                    .map(|_| ()),
                Input::Key {
                    key,
                    ctrl,
                    alt,
                    shift,
                    release,
                    repeat,
                } => {
                    let Some(event) = gui_key(&key, ctrl, alt, shift, release, repeat) else {
                        reject(client, &id, "invalid_key", "unrecognized terminal key");
                        return false;
                    };
                    app.with_input_source(Some(client_id), None, |app| {
                        app.forward_gui_key(pane_id, event)
                    });
                    Ok(())
                }
                input => private_input(app, client, &target, pane_id, foreground, input),
            };
            match result {
                Ok(()) => reply(
                    client,
                    Response::Reply {
                        id,
                        result: json!({"delivered":true}),
                    },
                ),
                Err(error) => reject(client, &id, "input_failed", error),
            }
            true
        }
    }
}

fn private_input(
    app: &App,
    client: &mut ClientState,
    target: &Target,
    pane_id: crate::ids::PaneId,
    foreground: bool,
    input: Input,
) -> Result<(), String> {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let pane = &app.panes[&pane_id];
    let state = client.app_surface.as_mut().unwrap();
    let terminal = target.terminal_id.as_ref().unwrap();
    let (cols, rows) = pane.size();
    if let Input::FocusEvent { focused } = input {
        let enabled = foreground
            && pane
                .engine
                .lock()
                .map_err(|_| "terminal unavailable")?
                .focus_reporting();
        if enabled {
            pane.try_send(if focused { b"\x1b[I" } else { b"\x1b[O" })?;
        }
        return Ok(());
    }
    if let Input::Scroll { lines } = input {
        let engine = pane.engine.lock().map_err(|_| "terminal unavailable")?;
        if state.offsets.len() >= 256 && !state.offsets.contains_key(terminal) {
            return Err("too many retained history viewports".into());
        }
        let viewport = state.offsets.entry(terminal.clone()).or_default();
        viewport.scroll(&*engine, lines);
        state.selection = None;
        return Ok(());
    }
    let geometry = match &input {
        Input::Mouse { geometry, .. }
        | Input::Selection { geometry, .. }
        | Input::Copy { geometry } => geometry,
        _ => return Err("invalid private input".into()),
    };
    let engine = pane.engine.lock().map_err(|_| "terminal unavailable")?;
    if engine.geometry_revision().to_string() != *geometry {
        state.mouse_buttons.clear();
        return Err("stale terminal geometry; wait for a fresh frame".into());
    }
    let offset = state
        .offsets
        .get_mut(terminal)
        .map_or(0, |viewport| viewport.resolve(&*engine));
    match input {
        Input::Copy { .. } => {
            let text = state
                .selection
                .as_ref()
                .filter(|(owner, _)| owner == target)
                .map(|(_, text)| text.clone())
                .ok_or("no selection for this terminal")?;
            drop(engine);
            super::send_private_clipboard(client, text);
        }
        Input::Selection {
            revision,
            mut start,
            mut end,
            mode,
            ..
        } => {
            if [start, end]
                .iter()
                .any(|point| point[0] >= rows || point[1] >= cols)
            {
                return Err("selection is outside terminal geometry".into());
            }
            // A selection addresses the exact displayed snapshot, not whatever
            // happens to occupy those coordinates after output/reflow/scroll.
            let current = format!(
                "{}:{}:{}:{offset}:{cols}:{rows}",
                pane.content_revision(),
                engine.output_generation(),
                engine.geometry_revision()
            );
            if revision != current
                || state
                    .terminals
                    .get(&target.pane_id)
                    .is_none_or(|(last, _, _)| last != &revision)
            {
                return Err("selection frame is stale".into());
            }
            if start > end {
                std::mem::swap(&mut start, &mut end);
            }
            let top = engine.history_len().saturating_sub(offset);
            if mode == SelectionMode::Line {
                start[1] = 0;
                end[1] = cols - 1;
            } else if mode == SelectionMode::Word {
                if let Some(layout) = engine.retained_row_layout(top + usize::from(start[0])) {
                    let whitespace = layout.is_whitespace(usize::from(start[1]));
                    while start[1] > 0
                        && layout.is_whitespace(usize::from(start[1] - 1)) == whitespace
                    {
                        start[1] -= 1;
                    }
                }
                if let Some(layout) = engine.retained_row_layout(top + usize::from(end[0])) {
                    let whitespace = layout.is_whitespace(usize::from(end[1]));
                    while end[1] + 1 < cols
                        && layout.is_whitespace(usize::from(end[1] + 1)) == whitespace
                    {
                        end[1] += 1;
                    }
                }
            }
            let text = engine
                .retained_selection_text((
                    (top + usize::from(start[0]), usize::from(start[1])),
                    (top + usize::from(end[0]), usize::from(end[1])),
                ))
                .ok_or("selection is no longer retained")?;
            if text.len() > crate::ipc::protocol::MAX_FRAME - 1024 {
                return Err("selection exceeds the clipboard budget".into());
            }
            state.selection = Some((target.clone(), text.clone()));
            if pane.mouse_options.copy_on_select {
                drop(engine);
                super::send_private_clipboard(client, text);
            }
        }
        Input::Mouse {
            action,
            button,
            row,
            col,
            ctrl,
            alt,
            shift,
            geometry,
        } => {
            if !foreground || offset != 0 || row >= rows || col >= cols {
                return Err("mouse target is passive, scrolled, or outside geometry".into());
            }
            if button == 2 && action == MouseAction::Press && !pane.mouse_options.right_click_to_app
            {
                return Err("right-click passthrough is disabled for this terminal".into());
            }
            drop(engine);
            let button_kind = match button {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                _ => MouseButton::Right,
            };
            let owned = state
                .mouse_buttons
                .get(&button)
                .is_some_and(|(owner, old)| owner == target && old == &geometry);
            let kind = match action {
                MouseAction::Press if button < 3 => MouseEventKind::Down(button_kind),
                MouseAction::Release if owned => MouseEventKind::Up(button_kind),
                MouseAction::Move if owned => MouseEventKind::Drag(button_kind),
                MouseAction::Move if button == 3 => MouseEventKind::Moved,
                MouseAction::WheelUp => MouseEventKind::ScrollUp,
                MouseAction::WheelDown => MouseEventKind::ScrollDown,
                _ => return Err("mouse gesture has no matching press".into()),
            };
            let mut modifiers = KeyModifiers::empty();
            modifiers.set(KeyModifiers::CONTROL, ctrl);
            modifiers.set(KeyModifiers::ALT, alt);
            modifiers.set(KeyModifiers::SHIFT, shift);
            let mut mode = pane.mouse_mode();
            mode.alternate_scroll &= pane.alt_screen();
            let Some(bytes) = App::gui_mouse_bytes(
                MouseEvent {
                    kind,
                    column: col,
                    row,
                    modifiers,
                },
                mode,
            ) else {
                return Ok(());
            };
            pane.try_send(&bytes)?;
            if action == MouseAction::Press {
                state
                    .mouse_buttons
                    .insert(button, (target.clone(), geometry));
            }
            if action == MouseAction::Release {
                state.mouse_buttons.remove(&button);
            }
        }
        _ => return Err("invalid private input".into()),
    }
    Ok(())
}

pub(super) fn dispatch_browsers(
    app: &mut App,
    clients: &super::Clients,
    revision: &mut Option<u64>,
) {
    let current = crate::ipc::api::current_sequence(&app.events);
    if *revision != Some(current) {
        *revision = Some(current);
        let invalid: Vec<_> = app
            .browser_control
            .bindings()
            .filter(|(_, client, target)| {
                !clients.contains_key(client) || app.validate_gui_target(target, false).is_err()
            })
            .map(|(id, _, _)| id.to_owned())
            .collect();
        for id in invalid {
            app.browser_control.revoke(&id);
        }
    }
    for (client_id, data) in app.browser_control.take_commands() {
        let Some(client) = clients
            .get(&client_id)
            .filter(|client| client.app_surface.is_some())
        else {
            app.browser_control.detach(client_id);
            continue;
        };
        reply(
            client,
            Response::Effect {
                kind: "browser_command".into(),
                data,
            },
        );
    }
}

pub(super) fn real_input(input: &Input) -> bool {
    match input {
        Input::Paste { text } => !text.is_empty(),
        Input::Key {
            key,
            release: false,
            repeat: false,
            ..
        } => key_code(key).is_some(),
        _ => false,
    }
}

fn key_code(key: &str) -> Option<KeyCode> {
    Some(match key {
        "Enter" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Escape" => KeyCode::Esc,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        value
            if value.starts_with('F')
                && value[1..]
                    .parse::<u8>()
                    .is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            KeyCode::F(value[1..].parse().ok()?)
        }
        value => {
            let mut chars = value.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

fn gui_key(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    release: bool,
    repeat: bool,
) -> Option<KeyEvent> {
    let mut code = key_code(key)?;
    if code == KeyCode::Tab && shift {
        code = KeyCode::BackTab;
    }
    let mut modifiers = KeyModifiers::empty();
    modifiers.set(KeyModifiers::CONTROL, ctrl);
    modifiers.set(KeyModifiers::ALT, alt);
    modifiers.set(KeyModifiers::SHIFT, shift);
    let kind = if release {
        KeyEventKind::Release
    } else if repeat {
        KeyEventKind::Repeat
    } else {
        KeyEventKind::Press
    };
    Some(KeyEvent::new_with_kind(code, modifiers, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> (ClientState, mpsc::Receiver<ServerMessage>) {
        let (messages, rx) = mpsc::channel();
        let mut client = ClientState::new(
            super::super::ClientSender {
                messages,
                frame_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            80,
            24,
            None,
            1,
            None,
        );
        client.app_surface = Some(State::default());
        (client, rx)
    }

    fn target(app: &App) -> Target {
        let pane = app.workspaces[0].tabs[0].layout.focus;
        Target {
            generation: app.backend_server_generation.clone(),
            workspace_id: app.workspaces[0].id.clone(),
            tab_id: app.workspaces[0].tabs[0].id.clone(),
            pane_id: pane.0.to_string(),
            terminal_id: Some(app.panes[&pane].terminal_runtime().unwrap().terminal_id),
        }
    }

    #[test]
    fn late_desktop_focus_and_keystrokes_cannot_reselect_the_previous_pane() {
        let _env = crate::persist::test_env("desktop-late-focus");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let old = target(&app);
        app.dispatch(
            "pane.split",
            &json!({"pane":old.pane_id,"direction":"right","focus":true}),
        )
        .unwrap();
        // A split gets its layout ID before the deferred PTY launch finishes.
        let current = app.workspaces[0].tabs[0].layout.focus;
        assert_ne!(old.pane_id, current.0.to_string());
        let (mut client, rx) = client();
        handle(
            &mut app,
            &mut client,
            1,
            true,
            Request::Focus {
                id: "blur".into(),
                target: old.clone(),
                activate: false,
            },
        );
        let ServerMessage::AppResponse(body) = rx.recv().unwrap() else {
            panic!("missing focus receipt")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["result"]["focused"],
            false
        );
        handle(
            &mut app,
            &mut client,
            1,
            true,
            Request::Input {
                id: "late".into(),
                target: old,
                input: Input::Paste {
                    text: "previous pane".into(),
                },
            },
        );
        let ServerMessage::AppResponse(body) = rx.recv().unwrap() else {
            panic!("missing key receipt")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["code"],
            "stale_target"
        );
        assert_eq!(app.workspaces[0].tabs[0].layout.focus, current);
    }

    #[test]
    fn registered_gui_owner_rechecks_ticket_before_touching_a_pty() {
        use crate::ipc::protocol::WorkspaceScope;
        let _env = crate::persist::test_env("registered-gui-owner-fence");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let target = target(&app);
        let (mut peer, rx) = client();
        peer.app_surface = None;
        peer.workspace_id = Some(target.workspace_id.clone());
        peer.projection = Some(super::super::ProjectionSubscription {
            active: true,
            epoch: 4,
            ..Default::default()
        });
        let scope = WorkspaceScope {
            server_generation: target.generation.clone(),
            workspace_id: target.workspace_id.clone(),
            epoch: 4,
        };
        let mut replay = |peer: &mut ClientState, scope: WorkspaceScope, target: Target, origin| {
            let request = Request::Input {
                id: "input".into(),
                target,
                input: Input::Paste { text: "x".into() },
            };
            owner_request(&mut app, peer, 1, scope.clone(), origin, request);
            let ServerMessage::WorkspaceApp {
                scope: echo,
                origin: source,
                body,
            } = rx.recv().unwrap()
            else {
                panic!("missing owner receipt")
            };
            assert_eq!(echo, scope);
            assert_eq!(source, origin);
            serde_json::from_str::<Value>(&body).unwrap()
        };
        for stale in [
            WorkspaceScope {
                epoch: 3,
                ..scope.clone()
            },
            WorkspaceScope {
                server_generation: "old".into(),
                ..scope.clone()
            },
            WorkspaceScope {
                workspace_id: "old".into(),
                ..scope.clone()
            },
        ] {
            assert_eq!(
                replay(&mut peer, stale, target.clone(), 8)["code"],
                "stale_target"
            );
        }
        let restarted = Target {
            terminal_id: Some("0".repeat(32)),
            ..target.clone()
        };
        assert_eq!(
            replay(&mut peer, scope.clone(), restarted, 8)["code"],
            "stale_target"
        );
        peer.prepared_workspace = Some(Box::new(client().0));
        assert_eq!(
            replay(&mut peer, scope.clone(), target.clone(), 8)["code"],
            "stale_target"
        );
        peer.prepared_workspace = None;
        peer.projection.as_mut().unwrap().active = false;
        assert_eq!(
            replay(&mut peer, scope.clone(), target.clone(), 8)["code"],
            "stale_target"
        );
        peer.projection.as_mut().unwrap().active = true;
        assert_eq!(
            replay(&mut peer, scope.clone(), target.clone(), 0)["code"],
            "invalid_request"
        );
        assert_eq!(
            replay(&mut peer, scope, target, 8)["result"]["accepted"],
            true
        );
    }

    #[test]
    fn registered_gui_late_receipt_cannot_complete_a_reused_request() {
        use crate::app::gui_projection::registered::Route;
        use crate::ids::PaneId;
        use crate::ipc::protocol::WorkspaceScope;
        let _env = crate::persist::test_env("registered-gui-late-receipt");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let (one, rx) = client();
        let (two, rx2) = client();
        let mut clients: super::super::Clients = [(1, one), (2, two)].into_iter().collect();
        let route = Route {
            connection: PaneId(77),
            generation: 3,
            workspace: "display".into(),
            scope: WorkspaceScope {
                server_generation: "boot".into(),
                workspace_id: "A".into(),
                epoch: 4,
            },
        };
        clients
            .get_mut(&1)
            .unwrap()
            .app_surface
            .as_mut()
            .unwrap()
            .registered
            .insert("same".into(), route.clone());
        let mut receive = |connection, generation, scope: &WorkspaceScope, origin| {
            remote_response(
                &mut app,
                &mut clients,
                connection,
                generation,
                scope,
                origin,
                Response::Reply {
                    id: "same".into(),
                    result: json!({"accepted":true}),
                },
            );
        };
        receive(route.connection, 2, &route.scope, 1);
        receive(
            route.connection,
            3,
            &WorkspaceScope {
                epoch: 3,
                ..route.scope.clone()
            },
            1,
        );
        receive(PaneId(88), 3, &route.scope, 1);
        receive(route.connection, 3, &route.scope, 2);
        assert!(rx.try_recv().is_err() && rx2.try_recv().is_err());
        receive(route.connection, 3, &route.scope, 1);
        // The receipt matches the caller, but the workspace has disappeared.
        let ServerMessage::AppResponse(body) = rx.recv().unwrap() else {
            panic!("missing error")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["code"],
            "stale_target"
        );
        receive(route.connection, 3, &route.scope, 1);
        assert!(rx.try_recv().is_err() && rx2.try_recv().is_err());
    }

    #[test]
    fn app_surface_private_history_and_returning_geometry_authority() {
        let _env = crate::persist::test_env("app-surface-private-geometry");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let target = target(&app);
        let pane = app.validate_gui_target(&target, true).unwrap().2;
        let (one, _one_rx) = client();
        let (two, _two_rx) = client();
        let mut clients = [(1, one), (2, two)].into_iter().collect();
        let mut foreground = Some(1);
        let mut activity = 10;
        let mut size = (80, 24);
        let request = |app: &mut App,
                       clients: &mut super::super::Clients,
                       foreground: &mut Option<u64>,
                       activity: &mut u64,
                       size: &mut (u16, u16),
                       id,
                       request| {
            super::super::apply(
                AppEvent::ClientAppRequest { id, request },
                app,
                clients,
                foreground,
                size,
                activity,
            );
        };
        for (id, cols, rows) in [(1, 80, 24), (2, 40, 10)] {
            request(
                &mut app,
                &mut clients,
                &mut foreground,
                &mut activity,
                &mut size,
                id,
                Request::Viewport {
                    id: "viewport".into(),
                    target: target.clone(),
                    cols,
                    rows,
                    cell_width: 9,
                    cell_height: 18,
                },
            );
        }
        assert_eq!(app.panes[&pane].size(), (80, 24));
        app.panes[&pane]
            .engine
            .lock()
            .unwrap()
            .advance("row\r\n".repeat(80).as_bytes());
        app.panes[&pane].engine.lock().unwrap().scroll(7);
        request(
            &mut app,
            &mut clients,
            &mut foreground,
            &mut activity,
            &mut size,
            2,
            Request::Input {
                id: "scroll".into(),
                target: target.clone(),
                input: Input::Scroll { lines: 5 },
            },
        );
        request(
            &mut app,
            &mut clients,
            &mut foreground,
            &mut activity,
            &mut size,
            2,
            Request::Focus {
                id: "focus".into(),
                target: target.clone(),
                activate: false,
            },
        );
        assert_eq!(foreground, Some(1));
        assert_eq!(app.panes[&pane].size(), (80, 24));
        assert_eq!(app.panes[&pane].engine.lock().unwrap().scroll_offset(), 7);
        let state = clients.get_mut(&2).unwrap().app_surface.as_mut().unwrap();
        assert_eq!(
            app.gui_terminal_frames(&mut state.offsets)[0].scroll_offset,
            5
        );
        let state = clients.get_mut(&1).unwrap().app_surface.as_mut().unwrap();
        assert_eq!(
            app.gui_terminal_frames(&mut state.offsets)[0].scroll_offset,
            0
        );
        // Fresh real input, not another resize callback, reclaims stored size.
        for (id, expected) in [(2, (40, 10)), (1, (80, 24))] {
            request(
                &mut app,
                &mut clients,
                &mut foreground,
                &mut activity,
                &mut size,
                id,
                Request::Input {
                    id: "press".into(),
                    target: target.clone(),
                    input: Input::Key {
                        key: "Escape".into(),
                        ctrl: false,
                        alt: false,
                        shift: false,
                        release: false,
                        repeat: false,
                    },
                },
            );
            assert_eq!(foreground, Some(id));
            assert_eq!(app.panes[&pane].size(), expected);
            assert_eq!(app.panes[&pane].cell_pixels(), Some((9, 18)));
            assert!(app.status[&pane].last_resize.is_some());
        }
        let mut stale = target;
        stale.terminal_id = Some("0".repeat(32));
        request(
            &mut app,
            &mut clients,
            &mut foreground,
            &mut activity,
            &mut size,
            2,
            Request::Input {
                id: "stale".into(),
                target: stale,
                input: Input::Paste {
                    text: "ignored".into(),
                },
            },
        );
        assert_eq!(foreground, Some(1));
        assert_eq!(app.panes[&pane].size(), (80, 24));
    }

    #[test]
    fn app_surface_browser_tab_and_mouse_preferences_preserve_exact_targets() {
        let _env = crate::persist::test_env("app-surface-desktop-controls");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let target = target(&app);
        let pane = app.validate_gui_target(&target, true).unwrap().2;
        let (mut client, rx) = client();
        assert!(handle(
            &mut app,
            &mut client,
            1,
            true,
            Request::MouseOptions {
                id: "mouse".into(),
                target: target.clone(),
                right_click_to_app: true,
                copy_on_select: false,
            },
        ));
        assert!(app.panes[&pane].mouse_options.right_click_to_app);
        assert!(!app.panes[&pane].mouse_options.copy_on_select);
        rx.recv().unwrap();
        let mut stale = target.clone();
        stale.terminal_id = Some("0".repeat(32));
        assert!(!handle(
            &mut app,
            &mut client,
            1,
            true,
            Request::MouseOptions {
                id: "stale".into(),
                target: stale,
                right_click_to_app: false,
                copy_on_select: true,
            },
        ));
        assert!(app.panes[&pane].mouse_options.right_click_to_app);
        assert!(!app.panes[&pane].mouse_options.copy_on_select);
        let ServerMessage::AppResponse(body) = rx.recv().unwrap() else {
            panic!("missing stale-target receipt")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["code"],
            "stale_target"
        );
        let terminals = app.panes.len();
        let tabs = app.workspaces[0].tabs.len();
        assert!(handle(
            &mut app,
            &mut client,
            1,
            true,
            Request::BrowserTab {
                id: "browser".into(),
                target: target.clone(),
                url: "https://example.invalid/".into(),
            },
        ));
        assert_eq!(app.panes.len(), terminals);
        assert_eq!(app.workspaces[0].tabs.len(), tabs + 1);
        assert!(app.panes.contains_key(&pane));
        let browser = app.workspaces[0].tabs.last().unwrap().layout.focus;
        assert!(matches!(
            app.views.get(&browser),
            Some(crate::app::ViewKind::Browser(_))
        ));
        assert_eq!(app.workspaces[0].active_tab, tabs);
        rx.recv().unwrap();
        let mut stale = target;
        stale.generation = "previous-owner".into();
        assert!(!handle(
            &mut app,
            &mut client,
            1,
            true,
            Request::BrowserTab {
                id: "stale-browser".into(),
                target: stale,
                url: "https://example.invalid/".into()
            },
        ));
        assert_eq!(app.workspaces[0].tabs.len(), tabs + 1);
    }

    #[test]
    fn app_surface_selection_copy_and_geometry_fences() {
        let _env = crate::persist::test_env("app-surface-selection");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let target = target(&app);
        let pane = app.validate_gui_target(&target, true).unwrap().2;
        app.panes
            .get_mut(&pane)
            .unwrap()
            .mouse_options
            .copy_on_select = false;
        app.resize_terminal_viewport(pane, 8, 3);
        let (tx, _rx) = mpsc::channel();
        let engine = crate::terminal::vt::create_engine(
            Default::default(),
            8,
            3,
            tx,
            64 * 1024,
            Default::default(),
        );
        engine
            .lock()
            .unwrap()
            .advance("  界e\u{301}abcX\r\nend".as_bytes());
        app.panes
            .get_mut(&pane)
            .unwrap()
            .replace_engine_for_test(engine.clone());
        let (mut client, rx) = client();
        let frame = app.gui_terminal_frames(&mut Default::default()).remove(0);
        client
            .app_surface
            .as_mut()
            .unwrap()
            .terminals
            .insert(target.pane_id.clone(), (frame.revision.clone(), 8, 3));
        private_input(
            &app,
            &mut client,
            &target,
            pane,
            false,
            Input::Selection {
                geometry: frame.geometry.clone(),
                revision: frame.revision.clone(),
                start: [1, 0],
                end: [0, 0],
                mode: SelectionMode::Cell,
            },
        )
        .unwrap();
        assert!(rx.try_recv().is_err(), "copy-on-select is disabled");
        private_input(
            &app,
            &mut client,
            &target,
            pane,
            false,
            Input::Copy {
                geometry: frame.geometry.clone(),
            },
        )
        .unwrap();
        assert!(
            matches!(rx.recv().unwrap(), ServerMessage::Clipboard(text) if text == "  界e\u{301}abcX")
        );
        app.panes
            .get_mut(&pane)
            .unwrap()
            .mouse_options
            .copy_on_select = true;
        private_input(
            &app,
            &mut client,
            &target,
            pane,
            false,
            Input::Selection {
                geometry: frame.geometry.clone(),
                revision: frame.revision.clone(),
                start: [1, 0],
                end: [0, 0],
                mode: SelectionMode::Cell,
            },
        )
        .unwrap();
        assert!(
            matches!(rx.recv().unwrap(), ServerMessage::Clipboard(text) if text == "  界e\u{301}abcX")
        );
        engine.lock().unwrap().advance(b"new");
        assert!(private_input(
            &app,
            &mut client,
            &target,
            pane,
            false,
            Input::Selection {
                geometry: frame.geometry.clone(),
                revision: frame.revision,
                start: [0, 0],
                end: [0, 1],
                mode: SelectionMode::Cell,
            }
        )
        .is_err());
        app.resize_terminal_viewport(pane, 9, 3);
        assert!(private_input(
            &app,
            &mut client,
            &target,
            pane,
            true,
            Input::Mouse {
                geometry: frame.geometry,
                action: MouseAction::Press,
                button: 0,
                row: 0,
                col: 0,
                ctrl: false,
                alt: false,
                shift: false,
            }
        )
        .is_err());
        assert_eq!(engine.lock().unwrap().scroll_offset(), 0);
    }

    #[test]
    fn app_surface_mouse_reuses_native_encoder_and_child_modes() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 2,
            modifiers: KeyModifiers::CONTROL,
        };
        let mut mode = crate::terminal::pty::MouseModes::default();
        assert!(App::gui_mouse_bytes(event, mode).is_none());
        mode.report = true;
        mode.sgr = true;
        assert_eq!(App::gui_mouse_bytes(event, mode).unwrap(), b"\x1b[<16;4;3M");
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..event
        };
        assert!(App::gui_mouse_bytes(moved, mode).is_none());
        mode.motion = true;
        assert_eq!(App::gui_mouse_bytes(moved, mode).unwrap(), b"\x1b[<51;4;3M");
        assert!(!real_input(&Input::Scroll { lines: 1 }));
        assert!(!real_input(&Input::Key {
            key: "unknown".into(),
            ctrl: false,
            alt: false,
            shift: false,
            release: false,
            repeat: false
        }));
    }
}
