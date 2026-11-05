//! The pooled display is the transport; an outer client never becomes an
//! owner client. Both ends check the committed ticket before accepting input.
use super::super::Clients;
use super::*;
use crate::app::gui_projection::registered::{Route, MAX_SCENE_BYTES};
use crate::ids::PaneId;
use crate::ipc::protocol::WorkspaceScope;

pub(in crate::ipc::server) fn owner_request(
    app: &mut App,
    client: &mut ClientState,
    id: u64,
    scope: WorkspaceScope,
    origin: u64,
    request: Request,
) -> bool {
    let request_id = request.id().to_owned();
    let mut changed = false;
    let result = (|| -> Result<Value, &'static str> {
        if client.version < 21
            || client.workspace_id.as_deref() != Some(&scope.workspace_id)
            || client.prepared_workspace.is_some()
            || scope.server_generation != app.backend_server_generation
            || !client
                .projection
                .as_ref()
                .is_some_and(|p| p.active && p.epoch == scope.epoch)
        {
            return Err("stale_target");
        }
        let wi = app
            .workspaces
            .iter()
            .position(|ws| ws.id == scope.workspace_id && ws.remote.is_none())
            .ok_or("stale_target")?;
        if matches!(request, Request::Snapshot { .. }) && origin == 0 {
            return app.owner_gui_scene(wi);
        }
        if origin == 0 {
            return Err("invalid_request");
        }
        if let Request::Graphic {
            target, sequence, ..
        } = &request
        {
            return app.gui_graphic(target, wi, sequence);
        }
        let target = match &request {
            Request::Focus { target, .. } | Request::Input { target, .. } => target,
            _ => return Err("unsupported_request"),
        };
        let (workspace, tab, pane) = app
            .validate_gui_target(target, matches!(request, Request::Input { .. }))
            .map_err(|_| "stale_target")?;
        if workspace != wi {
            return Err("stale_target");
        }
        let focused = app.workspaces[wi].active_tab == tab
            && app.workspaces[wi].tabs[tab].layout.focus == pane;
        match request {
            Request::Focus { activate, .. } => {
                if !focused && !activate {
                    return Err("stale_target");
                }
                if !focused {
                    let previous = app.active_ws;
                    app.active_ws = wi;
                    app.with_input_source(Some(id), Some(origin), |app| {
                        app.with_focus_events(|app| app.focus_location(wi, tab, pane))
                    });
                    app.active_ws = previous;
                    changed = true;
                }
            }
            Request::Input { input, .. } if focused => match input {
                Input::Key {
                    key,
                    ctrl,
                    alt,
                    shift,
                    release,
                    repeat,
                } => {
                    let key =
                        gui_key(&key, ctrl, alt, shift, release, repeat).ok_or("invalid_key")?;
                    app.with_input_source(Some(id), Some(origin), |app| {
                        app.forward_gui_key(pane, key)
                    });
                }
                Input::Paste { text } => {
                    app.panes[&pane]
                        .try_send_paste(&text)
                        .map_err(|_| "input_failed")?;
                }
                Input::FocusEvent { focused } => {
                    let p = &app.panes[&pane];
                    if p.engine
                        .lock()
                        .map_err(|_| "unavailable")?
                        .focus_reporting()
                    {
                        p.try_send(if focused { b"\x1b[I" } else { b"\x1b[O" })
                            .map_err(|_| "input_failed")?;
                    }
                }
                _ => return Err("unsupported_input"),
            },
            _ => return Err("stale_target"),
        }
        Ok(json!({"accepted":true}))
    })();
    let response = match result {
        Ok(result) => Response::Reply {
            id: request_id.clone(),
            result,
        },
        Err(code) => Response::Error {
            id: Some(request_id.clone()),
            code: code.into(),
            message: code.into(),
        },
    };
    let mut body = serde_json::to_string(&response).expect("projection response");
    if body.len() > MAX_SCENE_BYTES {
        body = serde_json::to_string(&Response::Error {
            id: Some(request_id),
            code: "limit_exceeded".into(),
            message: "workspace exceeds the App projection budget".into(),
        })
        .unwrap();
    }
    let _ = client.sender.send_control(ServerMessage::WorkspaceApp {
        scope,
        origin,
        body,
    });
    changed
}

pub(super) fn forward(
    app: &App,
    client: &mut ClientState,
    origin: u64,
    foreground: bool,
    request: &Request,
) -> Option<bool> {
    let target = match request {
        Request::Input { target, .. }
        | Request::Viewport { target, .. }
        | Request::Graphic { target, .. }
        | Request::Focus { target, .. } => target,
        _ => return None,
    };
    if !target.pane_id.starts_with("r:") {
        return None;
    }
    let Some((route, _)) = app.registered_target(target) else {
        reject(
            client,
            request.id(),
            "stale_target",
            "registered workspace selection changed",
        );
        return Some(false);
    };
    if matches!(request, Request::Viewport { .. }) {
        // Shared workspace displays own geometry as a whole. A per-leaf
        // ResizeObserver must not resize an inactive projection or a sibling PTY.
        reply(
            client,
            Response::Reply {
                id: request.id().into(),
                result: json!({"geometry_owner":false}),
            },
        );
        return Some(false);
    }
    if !foreground && !matches!(request, Request::Graphic { .. }) {
        reject(
            client,
            request.id(),
            "inactive_display",
            "display does not own input",
        );
        return Some(false);
    }
    let pending = &mut client.app_surface.as_mut()?.registered;
    if pending.len() >= MAX_PENDING || pending.contains_key(request.id()) {
        reject(
            client,
            request.id(),
            "busy",
            "too many pending owner requests",
        );
        return Some(false);
    }
    match app.send_registered_request(origin, request) {
        Ok(_) => {
            pending.insert(request.id().into(), route);
        }
        Err(code) => reject(
            client,
            request.id(),
            code,
            "registered workspace request rejected",
        ),
    }
    Some(false)
}

pub(super) fn expire(app: &App, client: &mut ClientState) {
    let current = app.registered_gui_binding().map(|(route, _, _)| route);
    let pending = &mut client.app_surface.as_mut().unwrap().registered;
    let stale: Vec<_> = pending
        .iter()
        .filter(|(_, r)| Some(*r) != current.as_ref())
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        client.app_surface.as_mut().unwrap().registered.remove(&id);
        reject(
            client,
            &id,
            "stale_target",
            "owner route expired; an earlier action may already have run",
        );
    }
}

pub(in crate::ipc::server) fn remote_response(
    app: &mut App,
    clients: &mut Clients,
    connection: PaneId,
    generation: u64,
    scope: &WorkspaceScope,
    origin: u64,
    response: Response,
) -> bool {
    if origin == 0 {
        return app.apply_registered_scene(connection, generation, scope, response);
    }
    let Some(client) = clients.get_mut(&origin) else {
        return false;
    };
    let id = match &response {
        Response::Reply { id, .. } | Response::Error { id: Some(id), .. } => id.clone(),
        _ => return false,
    };
    let Some(pending) = client.app_surface.as_mut().map(|s| &mut s.registered) else {
        return false;
    };
    let Some(route) = pending.get(&id) else {
        return false;
    };
    if route.connection != connection || route.generation != generation || &route.scope != scope {
        return false;
    }
    let route: Route = pending.remove(&id).unwrap();
    if app
        .registered_gui_binding()
        .is_some_and(|(current, _, _)| current == route)
    {
        reply(client, response);
    } else {
        reject(client, &id, "stale_target", "owner selection changed");
    }
    false
}
