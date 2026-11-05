//! Semantic views travel on the committed shared display, never on an SSH
//! connection selected by the renderer. A transition invalidates the whole
//! binding, including when it returns to the same workspace and owner IDs.
use super::{terminal_frame, views, ViewKind};
use crate::app::App;
use crate::ids::PaneId;
use crate::ipc::app_surface::protocol::{Input, Request, Response, Target, TerminalFrame};
use crate::ipc::protocol::{ClientMessage, WorkspaceScope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub(crate) const MAX_SCENE_BYTES: usize = 16 * 1024 * 1024;
const MAX_LEAVES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    pub connection: PaneId,
    pub generation: u64,
    pub workspace: String,
    pub scope: WorkspaceScope,
}

impl Route {
    fn namespace(&self, id: &str) -> String {
        format!(
            "r:{:x}:{:x}:{:x}:{}:{}",
            self.connection.0, self.generation, self.scope.epoch, self.scope.server_generation, id
        )
    }
}

#[derive(Default)]
pub(crate) struct DisplayState {
    pub display_size: Option<(u16, u16)>,
    route: Option<Route>,
    requested: Option<u64>,
    received: Option<u64>,
    pub topology: u64,
    scene: Option<Scene>,
    error: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Scene {
    tabs: Vec<Value>,
    pub frames: Vec<TerminalFrame>,
    #[serde(skip)]
    targets: HashMap<String, (Target, Target)>,
}

impl App {
    /// Read just the selected workspace. Do not serialize the owner's other
    /// workspaces, settings, credentials, or hidden terminal grids.
    pub(crate) fn owner_gui_scene(&self, wi: usize) -> Result<Value, &'static str> {
        let ws = &self.workspaces[wi];
        if ws.remote.is_some() || ws.tabs.len() > MAX_LEAVES {
            return Err("unavailable");
        }
        let mut scene = Scene {
            tabs: Vec::new(),
            frames: Vec::new(),
            targets: HashMap::new(),
        };
        let mut budget = views::Budget::default();
        let mut remaining = MAX_SCENE_BYTES / 2;
        let mut leaves = 0;
        for (ti, tab) in ws.tabs.iter().enumerate() {
            let active = ti == ws.active_tab;
            let mut panes = Vec::new();
            for id in tab.layout.leaves() {
                leaves += 1;
                if leaves > MAX_LEAVES {
                    return Err("limit_exceeded");
                }
                let terminal = self.panes.get(&id);
                let target = Target {
                    generation: self.backend_server_generation.clone(),
                    workspace_id: ws.id.clone(),
                    tab_id: tab.id.clone(),
                    pane_id: id.0.to_string(),
                    terminal_id: terminal
                        .and_then(|p| p.terminal_runtime())
                        .map(|r| r.terminal_id),
                };
                let mut leaf = if let Some(view) = self.views.get(&id) {
                    if matches!(view, ViewKind::Remote(_) | ViewKind::Browser(_)) {
                        views::unavailable("remote", "Owner view", "unsupported_projected_view")
                    } else {
                        views::native(view, &mut budget)
                    }
                } else {
                    json!({"kind":"terminal", "title":self.cached_pane_title(id)})
                };
                leaf["pane_id"] = json!(target.pane_id);
                leaf["focused"] = json!(active && id == tab.layout.focus);
                leaf["target"] = if tab.is_renameable()
                    && (target.terminal_id.is_some() || self.views.contains_key(&id))
                {
                    json!(target)
                } else {
                    Value::Null
                };
                if active {
                    if let Some(pane) = terminal.filter(|_| target.terminal_id.is_some()) {
                        let frame = terminal_frame(pane, target, None).ok_or("unavailable")?;
                        let bytes = serde_json::to_vec(&frame).map_err(|_| "unavailable")?.len();
                        remaining = remaining.checked_sub(bytes).ok_or("limit_exceeded")?;
                        scene.frames.push(frame);
                    }
                }
                panes.push(leaf);
            }
            scene.tabs.push(
                json!({"id":tab.id,"index":ti+1,"name":tab.name,"active":active,
                "kind":if tab.is_renameable() {"panes"} else {"owner_view"},
                "focus":tab.layout.focus.0.to_string(),"zoomed":false,
                "layout":tab.layout.to_tree(),"panes":panes}),
            );
        }
        serde_json::to_value(scene).map_err(|_| "unavailable")
    }

    pub(crate) fn refresh_registered_gui(&mut self) {
        let binding = self.registered_gui_binding();
        let route = binding.as_ref().map(|(r, _, _)| r);
        let cache = &mut self.registered_gui;
        if cache.route.as_ref() != route {
            cache.route = route.cloned();
            cache.scene = None;
            cache.requested = None;
            cache.received = None;
            cache.error = false;
            cache.topology = cache.topology.wrapping_add(1);
        }
        let Some((route, stamp, input)) = binding else {
            return;
        };
        if cache.requested.is_some() || cache.received == Some(stamp) {
            return;
        }
        let body = serde_json::to_string(&Request::Snapshot { id: "scene".into() })
            .expect("static request");
        if input
            .send(ClientMessage::WorkspaceApp {
                scope: route.scope,
                origin: 0,
                body,
            })
            .is_ok()
        {
            cache.requested = Some(stamp);
        }
    }

    pub(crate) fn apply_registered_scene(
        &mut self,
        connection: PaneId,
        generation: u64,
        scope: &WorkspaceScope,
        response: Response,
    ) -> bool {
        let Some((route, _, _)) = self.registered_gui_binding() else {
            return false;
        };
        let cache = &mut self.registered_gui;
        if route.connection != connection
            || route.generation != generation
            || &route.scope != scope
            || cache.route.as_ref() != Some(&route)
        {
            return false;
        }
        let result = match response {
            Response::Reply { id, result } if id == "scene" => Some(result),
            Response::Error { id: Some(id), .. } if id == "scene" => None,
            _ => return false,
        };
        let Some(stamp) = cache.requested.take() else {
            return false;
        };
        cache.received = Some(stamp);
        let scene = result
            .and_then(|r| serde_json::from_value::<Scene>(r).ok())
            .and_then(|s| s.bind(&route, &self.backend_server_generation));
        if cache.scene.as_ref().map(|s| &s.tabs) != scene.as_ref().map(|s| &s.tabs)
            || cache.error != scene.is_none()
        {
            cache.topology = cache.topology.wrapping_add(1);
        }
        cache.error = scene.is_none();
        cache.scene = scene;
        true
    }

    pub(crate) fn registered_scene(&self) -> Option<&Scene> {
        let (route, _, _) = self.registered_gui_binding()?;
        (self.registered_gui.route.as_ref() == Some(&route))
            .then_some(self.registered_gui.scene.as_ref())
            .flatten()
    }

    pub(crate) fn project_registered_workspace(&self, index: usize, out: &mut Value) {
        let scene = (index == self.active_ws)
            .then(|| self.registered_scene())
            .flatten();
        out["gui_available"] = json!(scene.is_some());
        out["projection_status"] = json!(if scene.is_some() {
            "ready"
        } else if self.registered_gui.error && index == self.active_ws {
            "unavailable"
        } else {
            self.registered_gui_status(index)
        });
        out["tabs"] = scene.map_or_else(|| json!([]), |s| json!(s.tabs));
    }

    pub(crate) fn registered_target(&self, target: &Target) -> Option<(Route, Target)> {
        let (route, _, _) = self.registered_gui_binding()?;
        let (display, owner) = self.registered_scene()?.targets.get(&target.pane_id)?;
        (display == target).then(|| (route, owner.clone()))
    }

    pub(crate) fn send_registered_request(
        &self,
        origin: u64,
        request: &Request,
    ) -> Result<Route, &'static str> {
        let (route, _, input) = self.registered_gui_binding().ok_or("stale_target")?;
        let mut request = request.clone();
        let target = match &mut request {
            Request::Input { target, input, .. } => {
                if !matches!(
                    input,
                    Input::Key { .. } | Input::Paste { .. } | Input::FocusEvent { .. }
                ) {
                    return Err("unsupported_input");
                }
                target
            }
            Request::Focus { target, .. } | Request::Graphic { target, .. } => target,
            _ => return Err("unsupported_request"),
        };
        *target = self.registered_target(target).ok_or("stale_target")?.1;
        input
            .send(ClientMessage::WorkspaceApp {
                scope: route.scope.clone(),
                origin,
                body: serde_json::to_string(&request).map_err(|_| "invalid_request")?,
            })
            .map_err(|_| "disconnected")?;
        Ok(route)
    }
}

impl Scene {
    fn bind(mut self, route: &Route, local_generation: &str) -> Option<Self> {
        if self.tabs.len() > MAX_LEAVES || self.frames.len() > MAX_LEAVES {
            return None;
        }
        let mut leaf_count = 0;
        let mut tab_ids = HashSet::new();
        for tab in &mut self.tabs {
            let tab_id = tab["id"].as_str()?.to_owned();
            if !tab_ids.insert(tab_id.clone()) {
                return None;
            }
            let mut leaves: HashSet<_> = tab["panes"]
                .as_array()?
                .iter()
                .map(|pane| pane["pane_id"].as_str()?.parse::<u32>().ok())
                .collect::<Option<_>>()?;
            let count = tab["panes"].as_array()?.len();
            leaf_count += count;
            if leaf_count > MAX_LEAVES
                || leaves.len() != count
                || !leaves.contains(&tab["focus"].as_str()?.parse().ok()?)
            {
                return None;
            }
            tab["id"] = json!(route.namespace(&tab_id));
            tab["focus"] = json!(route.namespace(tab["focus"].as_str()?));
            bind_layout(tab.get_mut("layout")?, route, &mut leaves, 0)?;
            if !leaves.is_empty() {
                return None;
            }
            for pane in tab["panes"].as_array_mut()? {
                let pane_id = pane["pane_id"].as_str()?.to_owned();
                pane["pane_id"] = json!(route.namespace(&pane_id));
                if pane["target"].is_null() {
                    continue;
                }
                let owner: Target = serde_json::from_value(pane["target"].take()).ok()?;
                owner.validate().ok()?;
                if owner.workspace_id != route.scope.workspace_id
                    || owner.generation != route.scope.server_generation
                    || owner.tab_id != tab_id
                    || owner.pane_id != pane_id
                    || self.targets.len() >= MAX_LEAVES
                {
                    return None;
                }
                let display = Target {
                    generation: local_generation.into(),
                    workspace_id: route.workspace.clone(),
                    tab_id: route.namespace(&tab_id),
                    pane_id: route.namespace(&pane_id),
                    terminal_id: owner.terminal_id.clone(),
                };
                display.validate().ok()?;
                if self
                    .targets
                    .insert(display.pane_id.clone(), (display.clone(), owner))
                    .is_some()
                {
                    return None;
                }
                pane["target"] = json!(display);
            }
        }
        for frame in &mut self.frames {
            let (display, owner) = self.targets.get(&route.namespace(&frame.target.pane_id))?;
            if owner != &frame.target
                || frame.target.terminal_id.is_none()
                || !(1..=super::MAX_ROWS).contains(&frame.rows)
                || !(1..=super::MAX_COLS).contains(&frame.cols)
                || frame.cells.len() > usize::from(frame.cols) * usize::from(frame.rows)
                || frame.graphics.len() > 128
                || frame.graphics.iter().map(|g| g.cells.len()).sum::<usize>()
                    > usize::from(frame.cols) * usize::from(frame.rows)
                || frame.graphics.iter().any(|g| {
                    !(1..=297).contains(&g.cols)
                        || !(1..=297).contains(&g.rows)
                        || g.sequence.parse::<u64>().is_err()
                        || g.cells.iter().any(|&[x, y, sx, sy]| {
                            x >= frame.cols
                                || y >= frame.rows
                                || u32::from(sx) >= g.cols
                                || u32::from(sy) >= g.rows
                        })
                })
            {
                return None;
            }
            frame.target = display.clone();
        }
        Some(self)
    }
}

fn bind_layout(
    tree: &mut Value,
    route: &Route,
    leaves: &mut HashSet<u32>,
    depth: usize,
) -> Option<()> {
    if depth > 64 {
        return None;
    }
    if let Some(id) = tree["Leaf"].as_u64() {
        if !leaves.remove(&u32::try_from(id).ok()?) {
            return None;
        }
        tree["Leaf"] = json!(route.namespace(&id.to_string()));
    } else {
        let split = tree.get_mut("Split")?;
        let axis = split.get("axis")?.as_u64()?;
        let ratio = split.get("ratio")?.as_f64()?;
        if axis > 1 || !(0.0..=1.0).contains(&ratio) {
            return None;
        }
        bind_layout(split.get_mut("a")?, route, leaves, depth + 1)?;
        bind_layout(split.get_mut("b")?, route, leaves, depth + 1)?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_gui_rejects_malformed_and_mismatched_layouts() {
        let route = Route {
            connection: PaneId(7),
            generation: 2,
            workspace: "local".into(),
            scope: WorkspaceScope {
                server_generation: "boot".into(),
                workspace_id: "owner".into(),
                epoch: 9,
            },
        };
        let bind = |layout| {
            serde_json::from_value::<Scene>(json!({"tabs":[{
            "id":"tab","focus":"1","panes":[{"pane_id":"1","target":null}],"layout":layout
        }],"frames":[]}))
            .unwrap()
            .bind(&route, "local")
        };
        assert!(bind(json!({"Leaf":1})).is_some());
        for bad in [
            Value::Null,
            json!({"Split":7}),
            json!({"Leaf":2}),
            json!({"Split":{"axis":0,"ratio":0.5,"a":{"Leaf":1},"b":{"Leaf":1}}}),
        ] {
            assert!(bind(bad).is_none());
        }
    }
}
