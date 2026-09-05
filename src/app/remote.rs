//! Managed remote-session federation.
//!
//! The local `App` remains the single owner of its UI. Each remote workspace is
//! represented by one native view whose cells come from the remote server's
//! existing binary display protocol. The remote server remains the sole writer
//! of that workspace, its tabs, panes, PTYs, and agent state.

use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use serde_json::{json, Value};

use super::{App, Cmd, Mode, Tab, ViewKind, Workspace};
use crate::event::AppEvent;
use crate::ids::PaneId;
use crate::ipc::protocol::{self, ClientMessage, FrameData, ServerMessage};
use crate::layout::TileLayout;
use crate::session::remote::{RemoteBinaryLocation, RemoteSession};

pub(super) struct RemoteWatcher {
    generation: u64,
    scope: Arc<crate::session::remote::ConnectionScope>,
    /// Only the first snapshot after an explicit discovery may reconnect an
    /// existing projection. Subsequent owner events never become idle retries.
    refresh_projections: bool,
}

impl Drop for RemoteWatcher {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RemoteWorkspaceRef {
    pub host: String,
    pub session: String,
    pub workspace_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAgentMeta {
    pub pane: String,
    pub agent: String,
    pub state: crate::ui::theme::State,
    pub tab: usize,
    pub focused: bool,
    pub name: Option<String>,
    pub session: Option<String>,
    pub cwd: String,
}

fn parse_remote_agents(workspace: &Value) -> Vec<RemoteAgentMeta> {
    workspace
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|tab| {
            tab.get("panes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |pane| (tab, pane))
        })
        .filter(|(_, pane)| {
            pane.get("is_agent")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || pane
                    .get("agent_session")
                    .is_some_and(|value| !value.is_null())
        })
        .filter_map(|(tab, pane)| {
            let state = match pane.get("agent_status").and_then(Value::as_str) {
                Some("blocked") => crate::ui::theme::State::Blocked,
                Some("working") => crate::ui::theme::State::Working,
                Some("done") => crate::ui::theme::State::Done,
                Some("idle") => crate::ui::theme::State::Idle,
                _ => crate::ui::theme::State::Unknown,
            };
            Some(RemoteAgentMeta {
                pane: pane.get("pane_id")?.as_str()?.into(),
                agent: pane.get("agent")?.as_str()?.into(),
                state,
                tab: tab.get("index").and_then(Value::as_u64).unwrap_or(1) as usize,
                focused: pane
                    .get("focused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                name: pane
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                session: pane
                    .get("agent_session")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                cwd: pane
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteWorkspaceMeta {
    pub agents: Vec<RemoteAgentMeta>,
    pub worktree: Option<crate::git::WorktreeMembership>,
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteSessionSnapshot {
    pub location: RemoteBinaryLocation,
    pub event_sequence: u64,
    pub workspaces: Vec<RemoteWorkspaceMeta>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteViewState {
    Connecting,
    Ready,
    Disconnected,
}

pub enum RemoteEffect {
    Notify(String),
    Sound(crate::sound::SoundSignal),
    Clipboard(String),
    OpenUrl(String),
}

pub struct RemoteView {
    pub agents: Vec<RemoteAgentMeta>,
    pub target: RemoteWorkspaceRef,
    pub state: RemoteViewState,
    pub error: Option<String>,
    pub frame: Option<FrameData>,
    pub generation: u64,
    pub input: Option<mpsc::Sender<ClientMessage>>,
    pub last_size: (u16, u16),
    effect_leader: Arc<AtomicBool>,
}

impl Drop for RemoteView {
    fn drop(&mut self) {
        if let Some(input) = &self.input {
            let _ = input.send(ClientMessage::Detach);
        }
    }
}

pub struct RemoteFrameSlot {
    latest: Mutex<Option<FrameData>>,
    pending: AtomicBool,
}

/// Reap the SSH bridge on every return path, including handshake and frame
/// decode failures. The input writer owns only the child's stdin pipe, so
/// terminating the process also lets that short-lived thread exit promptly.
struct RemoteChild(Child);

impl Drop for RemoteChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl RemoteFrameSlot {
    fn new() -> Self {
        Self {
            latest: Mutex::new(None),
            pending: AtomicBool::new(false),
        }
    }

    fn publish(
        self: &Arc<Self>,
        pane: PaneId,
        generation: u64,
        frame: FrameData,
        tx: &mpsc::Sender<AppEvent>,
    ) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(frame);
        }
        if self
            .pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = tx.send(AppEvent::RemoteFrameAvailable {
                pane,
                generation,
                slot: self.clone(),
            });
        }
    }

    pub(crate) fn take(&self) -> Option<FrameData> {
        // Clear first. A concurrent publisher then either queues a fresh event,
        // or its newer frame is the one taken below; an update cannot be stranded.
        self.pending.store(false, Ordering::Release);
        self.latest.lock().ok()?.take()
    }
}

impl App {
    pub(crate) fn finish_remote_workspace_picker(&mut self) -> bool {
        let Some(target) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.hosts.as_ref())
            .and_then(|hosts| hosts.connecting.clone())
        else {
            return false;
        };
        let Some(index) = self.workspaces.iter().enumerate().find_map(|(index, _)| {
            self.remote_workspace_view(index)
                .filter(|view| {
                    view.target.host == target.host
                        && view.target.session == target.session
                        && view.state == RemoteViewState::Ready
                })
                .map(|_| index)
        }) else {
            return false;
        };
        if self.send_workspace_remote(index, ClientMessage::Command("open_local_workspace".into()))
        {
            self.picker = None;
            self.active_ws = index;
            true
        } else {
            false
        }
    }

    pub(crate) fn remote_workspace_view(&self, index: usize) -> Option<&RemoteView> {
        let workspace = self.workspaces.get(index)?;
        workspace.remote.as_ref()?;
        match self.views.get(&workspace.tabs.first()?.layout.focus)? {
            ViewKind::Remote(view) => Some(view),
            _ => None,
        }
    }

    pub(crate) fn activate_remote_agent(
        &mut self,
        target: &RemoteWorkspaceRef,
        pane: &str,
        menu: bool,
    ) {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.remote.as_ref() == Some(target))
        else {
            return;
        };
        self.active_ws = index;
        let action = if menu {
            "remote_agent_menu"
        } else {
            "remote_agent_focus"
        };
        self.send_workspace_remote(index, ClientMessage::Command(format!("{action} {pane}")));
    }

    pub(crate) fn session_label(&self) -> String {
        if let Some(target) = crate::session::remote::view_target() {
            format!("{} [remote · {}]", target.session, target.host)
        } else {
            crate::session::display_name()
        }
    }

    pub(crate) fn remote_view_placeholder(&mut self, target: &RemoteSession) {
        let pane = PaneId::alloc();
        let reference = RemoteWorkspaceRef {
            host: target.host.clone(),
            session: target.session.clone(),
            workspace_id: String::new(),
        };
        self.views.insert(
            pane,
            ViewKind::Remote(RemoteView {
                agents: Vec::new(),
                target: reference.clone(),
                state: RemoteViewState::Connecting,
                error: None,
                frame: None,
                generation: u64::from(pane.0),
                input: None,
                last_size: (0, 0),
                effect_leader: Arc::new(AtomicBool::new(false)),
            }),
        );
        self.workspaces.push(Workspace {
            id: crate::ids::public_id("workspace"),
            name: target.session.clone(),
            cwd: PathBuf::new(),
            branch: None,
            git_ahead_behind: None,
            worktree: None,
            tabs: vec![Tab::panes(TileLayout::new(pane))],
            active_tab: 0,
            pinned: false,
            remote: Some(reference),
        });
        self.active_ws = 0;
    }

    pub(crate) fn start_merged_remote_sessions(&mut self) {
        if crate::session::remote::process_target().is_some() {
            return;
        }
        self.remote_registry_generation = self.remote_registry_generation.wrapping_add(1);
        let generation = self.remote_registry_generation;
        if self.config.remote_hosts.is_empty() {
            self.remote_host_status.clear();
            self.apply_remote_registry_loaded(
                generation,
                crate::session::remote::RemoteRegistry {
                    merge: Some(self.remote_merge_enabled),
                    ..Default::default()
                },
            );
            return;
        }
        self.remote_host_status = self
            .config
            .remote_hosts
            .iter()
            .map(|host| crate::session::remote::HostStatus {
                host: host.clone(),
                sessions: vec![],
                error: None,
            })
            .collect();
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let (registry, hosts) = crate::session::remote::discover_hosts();
            let _ = tx.send(AppEvent::RemoteRegistryLoaded {
                generation,
                registry,
                hosts,
            });
        });
    }

    pub(crate) fn apply_remote_registry_loaded(
        &mut self,
        generation: u64,
        registry: crate::session::remote::RemoteRegistry,
    ) {
        if generation != self.remote_registry_generation {
            return;
        }
        let session = crate::session::remote::view_target()
            .map_or_else(crate::session::display_name, |target| {
                target.session.clone()
            });
        self.remote_merge_enabled = registry.merge_enabled();
        if let Some(name) = self.remote_merge_switch_target(crate::session::remote::view_target()) {
            let tx = self.app_tx.clone();
            std::thread::spawn(move || {
                let result = crate::session::start_client_session(&name).and_then(|_| {
                    crate::session::remote::reload_local_session(&name)?;
                    Ok(Some(name))
                });
                let _ = tx.send(AppEvent::RemoteMergeChanged { generation, result });
            });
        }
        if let Some(menu) = self.named_session_menu.as_mut() {
            for row in &mut menu.rows {
                row.merged = registry.merge_enabled();
            }
        }
        let targets: Vec<_> = if let Some(target) = crate::session::remote::view_target() {
            if self.config.remote_hosts.contains(&target.host) {
                vec![target.clone()]
            } else {
                vec![]
            }
        } else {
            registry
                .for_merge(&session)
                .filter(|target| self.config.remote_hosts.contains(&target.host))
                .filter(|target| {
                    !self
                        .remote_host_status
                        .iter()
                        .any(|host| host.host == target.host && host.error.is_some())
                })
                .cloned()
                .collect()
        };
        self.remote_session_watchers
            .retain(|key, _| targets.iter().any(|target| target.canonical_name() == *key));
        let active_id = self
            .workspaces
            .get(self.active_ws)
            .map(|workspace| workspace.id.clone());
        let removed: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .filter_map(|(index, workspace)| {
                let remote = workspace.remote.as_ref()?;
                (!targets
                    .iter()
                    .any(|target| target.host == remote.host && target.session == remote.session))
                .then_some(index)
            })
            .collect();
        for index in removed.into_iter().rev() {
            self.close_workspace_after_rehome(index);
        }
        if let Some(index) = active_id.and_then(|id| {
            self.workspaces
                .iter()
                .position(|workspace| workspace.id == id)
        }) {
            self.active_ws = index;
        }
        for target in targets {
            self.discover_remote_session(target);
        }
        if let Some(target) = crate::session::remote::view_target() {
            if !self.config.remote_hosts.contains(&target.host) {
                self.apply_remote_session_discovered(
                    target.clone(),
                    Err(format!(
                        "SSH host `{}` is disabled in Settings > Remote",
                        target.host
                    )),
                );
            }
        }
    }

    fn remote_merge_switch_target(&self, view_target: Option<&RemoteSession>) -> Option<String> {
        // Registry reload also runs in detached proxy servers. Only an actual
        // client needs a prepared local handoff; background restart/config
        // maintenance must not start an otherwise stopped same-name session.
        (self.remote_merge_enabled && self.has_attached_client)
            .then_some(view_target)
            .flatten()
            .map(|target| target.session.clone())
    }

    pub(crate) fn toggle_remote_merge(&mut self) {
        let name = crate::session::remote::view_target()
            .map_or_else(crate::session::display_name, |target| {
                target.session.clone()
            });
        let enabled = !self.remote_merge_enabled;
        // Registry writes and host discovery are both user-triggered workers.
        let tx = self.app_tx.clone();
        self.remote_registry_generation = self.remote_registry_generation.wrapping_add(1);
        let generation = self.remote_registry_generation;
        std::thread::spawn(move || {
            let result = (|| {
                crate::session::remote::set_merge(&name, enabled)?;
                crate::session::remote::reload_local_sessions(Some(
                    &crate::session::display_name(),
                ))?;
                let (registry, hosts) = crate::session::remote::discover_hosts();
                let _ = tx.send(AppEvent::RemoteRegistryLoaded {
                    generation,
                    registry,
                    hosts,
                });
                // The registry result owns handoff preparation for both this
                // explicit toggle and CLI-triggered reloads. Starting here too
                // duplicates that lifecycle operation and SwitchSession event.
                Ok(None)
            })();
            let _ = tx.send(AppEvent::RemoteMergeChanged { generation, result });
        });
    }

    pub(crate) fn discover_remote_session(&mut self, target: RemoteSession) {
        let key = target.canonical_name();
        if !self.config.remote_hosts.contains(&target.host) {
            return;
        }
        let disconnected = self.views.values().any(|view| {
            matches!(view, ViewKind::Remote(view)
                if view.target.host == target.host
                    && view.target.session == target.session
                    && view.state == RemoteViewState::Disconnected)
        });
        if self.remote_session_watchers.contains_key(&key) && !disconnected {
            return;
        }
        // User-triggered refresh also replaces a healthy topology watcher when
        // its independent display bridge failed. Dropping it cancels its I/O;
        // the new generation fences any snapshot already queued by that watcher.
        self.remote_session_watchers.remove(&key);
        self.remote_watcher_generation = self.remote_watcher_generation.wrapping_add(1);
        let generation = self.remote_watcher_generation;
        let scope = Arc::new(crate::session::remote::ConnectionScope::default());
        self.remote_session_watchers.insert(
            key,
            RemoteWatcher {
                generation,
                scope: scope.clone(),
                refresh_projections: true,
            },
        );
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let result = remote_snapshot(&target, &scope);
            let Ok(snapshot) = result else {
                let _ = tx.send(AppEvent::RemoteSessionDiscovered {
                    generation,
                    target,
                    result,
                });
                return;
            };
            let sequence = snapshot.event_sequence;
            let location = snapshot.location;
            if tx
                .send(AppEvent::RemoteSessionDiscovered {
                    generation,
                    target: target.clone(),
                    result: Ok(snapshot),
                })
                .is_err()
            {
                return;
            }
            if let Err(error) =
                watch_remote_session(&target, location, sequence, generation, &scope, &tx)
            {
                let _ = tx.send(AppEvent::RemoteSessionWatcherClosed {
                    generation,
                    target,
                    error,
                });
            }
        });
    }

    pub(crate) fn remote_watcher_is_current(
        &self,
        target: &RemoteSession,
        generation: u64,
    ) -> bool {
        self.remote_session_watchers
            .get(&target.canonical_name())
            .is_some_and(|watcher| watcher.generation == generation)
    }

    pub(crate) fn apply_remote_session_discovered(
        &mut self,
        target: RemoteSession,
        result: Result<RemoteSessionSnapshot, String>,
    ) {
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(picker) = self.picker.as_mut() {
                    if let Some(choices) = picker
                        .hosts
                        .as_mut()
                        .filter(|choices| choices.connecting.as_ref() == Some(&target))
                    {
                        choices.connecting = None;
                        picker.error = Some(error.clone());
                    }
                }
                self.remote_session_watchers
                    .remove(&target.canonical_name());
                for view in self.views.values_mut() {
                    if let ViewKind::Remote(view) = view {
                        if view.target.host == target.host && view.target.session == target.session
                        {
                            view.state = RemoteViewState::Disconnected;
                            view.error = Some(error.clone());
                        }
                    }
                }
                self.show_toast(format!("{}: {error}", target.canonical_name()));
                return;
            }
        };
        let active_id = self
            .workspaces
            .get(self.active_ws)
            .map(|workspace| workspace.id.clone());
        let refresh_projections = self
            .remote_session_watchers
            .get_mut(&target.canonical_name())
            .is_some_and(|watcher| std::mem::take(&mut watcher.refresh_projections));
        let mut reconnect = Vec::new();
        let mut metadata = snapshot.workspaces;
        // A live session may have no workspace. Keep a native empty-session
        // projection so its folder picker can still create the first one.
        if metadata.is_empty()
            && (crate::session::remote::view_target() == Some(&target)
                || self
                    .picker
                    .as_ref()
                    .and_then(|picker| picker.hosts.as_ref())
                    .and_then(|hosts| hosts.connecting.as_ref())
                    == Some(&target))
        {
            metadata.push(RemoteWorkspaceMeta {
                id: String::new(),
                name: target.session.clone(),
                cwd: String::new(),
                branch: None,
                worktree: None,
                agents: Vec::new(),
            });
        }
        self.closed_remote_workspaces.retain(|remote| {
            remote.host != target.host
                || remote.session != target.session
                || metadata.iter().any(|meta| meta.id == remote.workspace_id)
        });
        let mut removed = Vec::new();
        for (index, workspace) in self.workspaces.iter_mut().enumerate() {
            let Some(remote) = workspace.remote.as_ref() else {
                continue;
            };
            if remote.host != target.host || remote.session != target.session {
                continue;
            }
            if let Some(position) = metadata
                .iter()
                .position(|meta| meta.id == remote.workspace_id)
            {
                let meta = metadata.swap_remove(position);
                workspace.name = meta.name;
                workspace.cwd = PathBuf::from(meta.cwd);
                workspace.branch = meta.branch;
                workspace.worktree = meta.worktree;
                if let Some(pane) = workspace.tabs.first().map(|tab| tab.layout.focus) {
                    if let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) {
                        view.agents = meta.agents;
                        if refresh_projections && view.state == RemoteViewState::Disconnected {
                            if let Some(input) = view.input.take() {
                                let _ = input.send(ClientMessage::Detach);
                            }
                            view.generation = view.generation.wrapping_add(1);
                            view.state = RemoteViewState::Connecting;
                            view.error = None;
                            reconnect.push((
                                pane,
                                view.generation,
                                remote.workspace_id.clone(),
                                view.effect_leader.clone(),
                            ));
                        }
                    }
                }
            } else {
                removed.push(index);
            }
        }
        for index in removed.into_iter().rev() {
            self.close_workspace_after_rehome(index);
        }
        if let Some(active_id) = active_id {
            if let Some(index) = self
                .workspaces
                .iter()
                .position(|workspace| workspace.id == active_id)
            {
                self.active_ws = index;
            }
        }
        metadata.retain(|meta| {
            !self.closed_remote_workspaces.contains(&RemoteWorkspaceRef {
                host: target.host.clone(),
                session: target.session.clone(),
                workspace_id: meta.id.clone(),
            })
        });
        for meta in metadata {
            self.add_remote_workspace(&target, meta, snapshot.location);
        }
        for (pane, generation, workspace_id, effect_leader) in reconnect {
            spawn_projection(
                pane,
                generation,
                target.clone(),
                workspace_id,
                snapshot.location,
                effect_leader,
                self.app_tx.clone(),
            );
        }
        if self.workspaces.iter().any(|workspace| {
            workspace.remote.as_ref().is_some_and(|remote| {
                remote.host == target.host
                    && remote.session == target.session
                    && !remote.workspace_id.is_empty()
            })
        }) {
            let placeholders: Vec<_> = self
                .workspaces
                .iter()
                .enumerate()
                .filter(|(_, workspace)| {
                    workspace.remote.as_ref().is_some_and(|remote| {
                        remote.host == target.host
                            && remote.session == target.session
                            && remote.workspace_id.is_empty()
                    })
                })
                .map(|(index, _)| index)
                .collect();
            for index in placeholders.into_iter().rev() {
                self.close_workspace_after_rehome(index);
            }
        }
        self.rebalance_remote_effect_leader(&target);
        self.finish_remote_workspace_picker();
    }

    pub(crate) fn apply_remote_session_watcher_closed(
        &mut self,
        target: RemoteSession,
        error: String,
    ) {
        self.remote_session_watchers
            .remove(&target.canonical_name());
        for view in self.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                if view.target.host == target.host && view.target.session == target.session {
                    for agent in &mut view.agents {
                        agent.state = crate::ui::theme::State::Unknown;
                    }
                }
            }
        }
        self.show_toast(format!(
            "{} {}: {error}",
            target.canonical_name(),
            self.catalog.remote_topology_stopped
        ));
    }

    fn add_remote_workspace(
        &mut self,
        target: &RemoteSession,
        meta: RemoteWorkspaceMeta,
        location: RemoteBinaryLocation,
    ) {
        let pane = PaneId::alloc();
        let generation = u64::from(pane.0);
        let remote = RemoteWorkspaceRef {
            host: target.host.clone(),
            session: target.session.clone(),
            workspace_id: meta.id.clone(),
        };
        let effect_leader = Arc::new(AtomicBool::new(false));
        self.views.insert(
            pane,
            ViewKind::Remote(RemoteView {
                agents: meta.agents,
                target: remote.clone(),
                state: RemoteViewState::Connecting,
                error: None,
                frame: None,
                generation,
                input: None,
                last_size: (80, 24),
                effect_leader: effect_leader.clone(),
            }),
        );
        self.workspaces.push(Workspace {
            id: crate::ids::public_id("workspace"),
            name: meta.name,
            cwd: PathBuf::from(meta.cwd),
            branch: meta.branch,
            git_ahead_behind: None,
            worktree: meta.worktree,
            tabs: vec![Tab::panes(TileLayout::new(pane))],
            active_tab: 0,
            pinned: false,
            remote: Some(remote.clone()),
        });
        spawn_projection(
            pane,
            generation,
            target.clone(),
            remote.workspace_id,
            location,
            effect_leader,
            self.app_tx.clone(),
        );
    }

    fn rebalance_remote_effect_leader(&mut self, target: &RemoteSession) {
        let leader = self
            .views
            .iter()
            .find_map(|(pane, view)| match view {
                ViewKind::Remote(view)
                    if view.target.host == target.host
                        && view.target.session == target.session
                        && view.state == RemoteViewState::Ready =>
                {
                    Some(*pane)
                }
                _ => None,
            })
            .or_else(|| {
                self.views.iter().find_map(|(pane, view)| match view {
                    ViewKind::Remote(view)
                        if view.target.host == target.host
                            && view.target.session == target.session
                            && view.state != RemoteViewState::Disconnected =>
                    {
                        Some(*pane)
                    }
                    _ => None,
                })
            });
        for (pane, view) in &mut self.views {
            let ViewKind::Remote(view) = view else {
                continue;
            };
            if view.target.host == target.host && view.target.session == target.session {
                view.effect_leader
                    .store(Some(*pane) == leader, Ordering::Release);
            }
        }
    }

    pub(super) fn rebalance_remote_effect_leaders(&mut self) {
        let mut leaders = std::collections::HashMap::new();
        for (pane, view) in &self.views {
            let ViewKind::Remote(view) = view else {
                continue;
            };
            if view.state == RemoteViewState::Ready {
                leaders
                    .entry((view.target.host.clone(), view.target.session.clone()))
                    .or_insert(*pane);
            }
        }
        for (pane, view) in &self.views {
            let ViewKind::Remote(view) = view else {
                continue;
            };
            if view.state != RemoteViewState::Disconnected {
                leaders
                    .entry((view.target.host.clone(), view.target.session.clone()))
                    .or_insert(*pane);
            }
        }
        for (pane, view) in &mut self.views {
            let ViewKind::Remote(view) = view else {
                continue;
            };
            let leader = leaders
                .get(&(view.target.host.clone(), view.target.session.clone()))
                .is_some_and(|leader| *leader == *pane);
            view.effect_leader.store(leader, Ordering::Release);
        }
    }

    pub(crate) fn apply_remote_projection_ready(
        &mut self,
        pane: PaneId,
        generation: u64,
        input: mpsc::Sender<ClientMessage>,
    ) {
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if view.generation != generation {
            return;
        }
        view.input = Some(input);
        view.state = RemoteViewState::Ready;
        view.error = None;
        // The new owner connection starts at its handshake viewport. A render
        // while connecting may already have cached our desired size without
        // an input sender, so force it to be sent after this handshake.
        view.last_size = (0, 0);
        self.rebalance_remote_effect_leaders();
    }

    pub(crate) fn apply_remote_frame(
        &mut self,
        pane: PaneId,
        generation: u64,
        slot: &RemoteFrameSlot,
    ) {
        let Some(frame) = slot.take() else {
            return;
        };
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if view.generation == generation {
            view.frame = Some(frame);
            view.state = RemoteViewState::Ready;
            view.error = None;
        }
    }

    pub(crate) fn apply_remote_projection_closed(
        &mut self,
        pane: PaneId,
        generation: u64,
        error: String,
    ) {
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if view.generation == generation {
            view.state = RemoteViewState::Disconnected;
            view.input = None;
            view.error = Some(error);
        }
        self.rebalance_remote_effect_leaders();
    }

    pub(crate) fn apply_remote_effect(&mut self, effect: RemoteEffect) {
        match effect {
            RemoteEffect::Notify(message) => self.pending_notify.push(message),
            RemoteEffect::Sound(signal) => self.pending_sound = Some(signal),
            RemoteEffect::Clipboard(text) => self.pending_clipboard = Some(text),
            RemoteEffect::OpenUrl(url) => self.pending_open_url = Some(url),
        }
    }

    pub(crate) fn active_remote_pane(&self) -> Option<PaneId> {
        let workspace = self.workspaces.get(self.active_ws)?;
        workspace.remote.as_ref()?;
        workspace
            .tabs
            .get(workspace.active_tab)
            .map(|tab| tab.layout.focus)
    }

    pub(crate) fn send_active_remote(&self, message: ClientMessage) -> bool {
        self.active_remote_pane()
            .and_then(|pane| self.views.get(&pane))
            .and_then(|view| match view {
                ViewKind::Remote(view) => view.input.as_ref(),
                _ => None,
            })
            .is_some_and(|input| input.send(message).is_ok())
    }

    pub(crate) fn send_workspace_remote(&self, index: usize, message: ClientMessage) -> bool {
        self.workspaces
            .get(index)
            .and_then(|ws| ws.tabs.get(ws.active_tab))
            .and_then(|tab| self.views.get(&tab.layout.focus))
            .and_then(|view| match view {
                ViewKind::Remote(view) => view.input.as_ref(),
                _ => None,
            })
            .is_some_and(|input| input.send(message).is_ok())
    }

    pub(crate) fn handle_active_remote_key(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
    ) -> Option<bool> {
        self.active_remote_pane()?;
        if self.mode == Mode::Normal {
            if self.prefix.matches(&key) {
                self.mode = Mode::Prefix;
                return Some(true);
            }
            if let Some(command) = super::keys::direct_command(&self.direct_keymap, &key) {
                if outer_command(command) {
                    self.run_cmd(command);
                } else {
                    let _ =
                        self.send_active_remote(ClientMessage::Command(command.id().to_string()));
                }
                return Some(true);
            }
            let _ = self.send_active_remote(ClientMessage::Key(key));
            return Some(false);
        }

        if self.mode == Mode::Prefix {
            self.mode = Mode::Normal;
            let prefix = self.prefix.key_event();
            if self.prefix.matches(&key) {
                let _ = self.send_active_remote(ClientMessage::Key(prefix));
                let _ = self.send_active_remote(ClientMessage::Key(key));
                return Some(true);
            }
            let command = super::keys::key_string(&key)
                .and_then(|binding| self.keymap.get(&binding).copied());
            if command.is_some_and(outer_command) {
                self.run_cmd(command.expect("checked above"));
            } else if let Some(command) = command {
                let _ = self.send_active_remote(ClientMessage::Command(command.id().to_string()));
            }
            return Some(true);
        }
        Some(false)
    }

    pub(crate) fn forward_active_remote_mouse(
        &self,
        mut mouse: ratatui::crossterm::event::MouseEvent,
    ) -> bool {
        let Some(pane) = self.active_remote_pane() else {
            return false;
        };
        let Some(rect) = self
            .pane_content_rects
            .iter()
            .find_map(|(candidate, rect)| (*candidate == pane).then_some(*rect))
        else {
            return false;
        };
        if mouse.column < rect.x
            || mouse.column >= rect.right()
            || mouse.row < rect.y
            || mouse.row >= rect.bottom()
        {
            return false;
        }
        mouse.column -= rect.x;
        mouse.row -= rect.y;
        self.send_active_remote(ClientMessage::Mouse(mouse))
    }

    pub(crate) fn resize_active_remote_projection(&mut self) {
        let Some(pane) = self.active_remote_pane() else {
            return;
        };
        let Some(rect) = self
            .pane_content_rects
            .iter()
            .find_map(|(candidate, rect)| (*candidate == pane).then_some(*rect))
        else {
            return;
        };
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        let size = (rect.width.max(1), rect.height.max(1));
        if size == view.last_size {
            return;
        }
        view.last_size = size;
        if let Some(input) = &view.input {
            let _ = input.send(ClientMessage::Resize {
                cols: size.0,
                rows: size.1,
            });
        }
    }
}

fn outer_command(command: Cmd) -> bool {
    matches!(
        command,
        Cmd::NewWorkspace
            | Cmd::CloseWorkspace
            | Cmd::NextWorkspace
            | Cmd::PrevWorkspace
            | Cmd::JumpWorkspace(_)
            | Cmd::OpenSettings
            | Cmd::OpenSessions
            | Cmd::ToggleSidebar
            | Cmd::ToggleRightSidebar
            | Cmd::Detach
    )
}

fn remote_snapshot(
    target: &RemoteSession,
    scope: &crate::session::remote::ConnectionScope,
) -> Result<RemoteSessionSnapshot, String> {
    let location = crate::session::remote::verify_remote_version(&target.host)?;
    remote_snapshot_at(target, location, scope)
}

fn remote_snapshot_at(
    target: &RemoteSession,
    location: RemoteBinaryLocation,
    scope: &crate::session::remote::ConnectionScope,
) -> Result<RemoteSessionSnapshot, String> {
    let mut connection =
        crate::session::remote::connect_control_at(target, location)?.in_scope(scope)?;
    writeln!(
        connection,
        "{}",
        json!({"id":"remote-session-discovery","method":"session.snapshot","params":{}})
    )
    .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(connection);
    let response =
        crate::ipc::api::read_response_frame(&mut reader).map_err(|error| error.to_string())?;
    let response: Value = serde_json::from_str(&response).map_err(|error| error.to_string())?;
    parse_remote_snapshot(&response, location)
}

fn parse_remote_snapshot(
    response: &Value,
    location: RemoteBinaryLocation,
) -> Result<RemoteSessionSnapshot, String> {
    if let Some(error) = response.get("error") {
        return Err(format!("remote snapshot failed: {error}"));
    }
    let workspaces = response
        .get("result")
        .and_then(|result| result.get("workspaces"))
        .and_then(Value::as_array)
        .ok_or_else(|| "remote snapshot did not contain workspaces".to_string())?;
    let event_sequence = response
        .get("result")
        .and_then(|result| result.get("event_sequence"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "remote snapshot did not contain an event sequence".to_string())?;
    let workspaces = workspaces
        .iter()
        // A remote host may itself have merge configured. Federation is only
        // one level deep: project that host's own workspaces and leave its
        // separately registered remotes to their actual owner.
        .filter(|workspace| workspace.get("host").is_none_or(Value::is_null))
        .map(|workspace| {
            Ok(RemoteWorkspaceMeta {
                agents: parse_remote_agents(workspace),
                worktree: workspace
                    .get("worktree")
                    .filter(|value| !value.is_null())
                    .map(|value| crate::git::WorktreeMembership {
                        common_dir: PathBuf::from(
                            value
                                .get("common_dir")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                        linked: value
                            .get("linked")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }),
                id: workspace
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "remote workspace has no stable id".to_string())?
                    .to_string(),
                name: workspace
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("workspace")
                    .to_string(),
                cwd: workspace
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                branch: workspace
                    .get("branch")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(RemoteSessionSnapshot {
        location,
        event_sequence,
        workspaces,
    })
}

fn watch_remote_session(
    target: &RemoteSession,
    location: RemoteBinaryLocation,
    mut after_sequence: u64,
    generation: u64,
    scope: &crate::session::remote::ConnectionScope,
    app_tx: &mpsc::Sender<AppEvent>,
) -> Result<(), String> {
    loop {
        let mut connection =
            crate::session::remote::connect_control_at(target, location)?.in_scope(scope)?;
        writeln!(
            connection,
            "{}",
            json!({
                "id":"remote-session-events",
                "method":"events.subscribe",
                "params":{"after_sequence":after_sequence},
            })
        )
        .map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(connection);
        let response =
            crate::ipc::api::read_response_frame(&mut reader).map_err(|error| error.to_string())?;
        let response: Value = serde_json::from_str(&response).map_err(|error| error.to_string())?;
        if let Some(error) = response.get("error") {
            if error.get("code").and_then(Value::as_str) == Some("resync_required") {
                let snapshot = remote_snapshot_at(target, location, scope)?;
                after_sequence = snapshot.event_sequence;
                app_tx
                    .send(AppEvent::RemoteSessionDiscovered {
                        generation,
                        target: target.clone(),
                        result: Ok(snapshot),
                    })
                    .map_err(|_| "local session closed".to_string())?;
                continue;
            }
            return Err(format!("remote event subscription failed: {error}"));
        }

        loop {
            let line = crate::ipc::api::read_stream_frame(&mut reader)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "remote event subscription closed".to_string())?;
            let event: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
            let name = event
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name == "events.resync_required" {
                let snapshot = remote_snapshot_at(target, location, scope)?;
                after_sequence = snapshot.event_sequence;
                app_tx
                    .send(AppEvent::RemoteSessionDiscovered {
                        generation,
                        target: target.clone(),
                        result: Ok(snapshot),
                    })
                    .map_err(|_| "local session closed".to_string())?;
                break;
            }
            if matches!(
                name,
                "workspace.created"
                    | "workspace.closed"
                    | "workspace.renamed"
                    | "workspace.metadata_reported"
                    | "pane.agent_status_changed"
                    | "pane.created"
                    | "pane.closed"
                    | "pane.moved"
                    | "pane.focused"
                    | "tab.created"
                    | "tab.closed"
                    | "tab.renamed"
                    | "tab.focused"
                    | "terminal.metadata_changed"
                    | "terminal.moved"
            ) {
                let snapshot = remote_snapshot_at(target, location, scope)?;
                app_tx
                    .send(AppEvent::RemoteSessionDiscovered {
                        generation,
                        target: target.clone(),
                        result: Ok(snapshot),
                    })
                    .map_err(|_| "local session closed".to_string())?;
            }
        }
    }
}

fn spawn_projection(
    pane: PaneId,
    generation: u64,
    target: RemoteSession,
    workspace_id: String,
    location: RemoteBinaryLocation,
    effect_leader: Arc<AtomicBool>,
    app_tx: mpsc::Sender<AppEvent>,
) {
    std::thread::spawn(move || {
        let result = run_projection(
            pane,
            generation,
            &target,
            &workspace_id,
            location,
            effect_leader,
            &app_tx,
        );
        let error = result
            .err()
            .unwrap_or_else(|| "remote projection closed".to_string());
        let _ = app_tx.send(AppEvent::RemoteProjectionClosed {
            pane,
            generation,
            error,
        });
    });
}

fn run_projection(
    pane: PaneId,
    generation: u64,
    target: &RemoteSession,
    workspace_id: &str,
    location: RemoteBinaryLocation,
    effect_leader: Arc<AtomicBool>,
    app_tx: &mpsc::Sender<AppEvent>,
) -> Result<(), String> {
    let mut command =
        crate::session::remote::bridge_command(target, "remote-client-bridge", location);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = RemoteChild(command.spawn().map_err(|error| error.to_string())?);
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| "SSH bridge has no stdin".to_string())?;
    let output = child
        .0
        .stdout
        .take()
        .ok_or_else(|| "SSH bridge has no stdout".to_string())?;
    protocol::write_message(
        &mut input,
        &if workspace_id.is_empty() {
            ClientMessage::Hello {
                version: protocol::PROTOCOL_VERSION,
                cols: 80,
                rows: 24,
            }
        } else {
            ClientMessage::HelloWorkspace {
                version: protocol::PROTOCOL_VERSION,
                cols: 80,
                rows: 24,
                workspace_id: workspace_id.to_string(),
            }
        },
    )
    .map_err(|error| error.to_string())?;
    let mut output = BufReader::new(output);
    match protocol::read_message::<_, ServerMessage>(&mut output)
        .map_err(|error| error.to_string())?
    {
        ServerMessage::Welcome { error: None, .. } => {}
        ServerMessage::Welcome {
            error: Some(error), ..
        } => return Err(error),
        _ => return Err("unexpected remote projection handshake".to_string()),
    }
    let probe_terminal = match protocol::read_message::<_, ServerMessage>(&mut output)
        .map_err(|error| error.to_string())?
    {
        ServerMessage::Ready { probe_terminal } => probe_terminal,
        _ => return Err("unexpected remote projection negotiation".to_string()),
    };
    if probe_terminal {
        protocol::write_message(&mut input, &ClientMessage::TerminalColors(None))
            .map_err(|error| error.to_string())?;
    }

    let (input_tx, input_rx) = mpsc::channel::<ClientMessage>();
    app_tx
        .send(AppEvent::RemoteProjectionReady {
            pane,
            generation,
            input: input_tx,
        })
        .map_err(|_| "local session closed".to_string())?;
    std::thread::spawn(move || {
        for message in input_rx {
            let detach = matches!(message, ClientMessage::Detach);
            if protocol::write_message(&mut input, &message).is_err() || detach {
                break;
            }
        }
    });

    let slot = Arc::new(RemoteFrameSlot::new());
    let mut frame = None;
    loop {
        match protocol::read_message::<_, ServerMessage>(&mut output) {
            Ok(ServerMessage::Frame(next)) => {
                frame = Some(next.clone());
                slot.publish(pane, generation, next, app_tx);
            }
            Ok(ServerMessage::FrameDiff(diff)) => {
                let Some(current) = frame.as_mut() else {
                    return Err("remote projection sent a diff before its full frame".to_string());
                };
                protocol::apply_diff(current, &diff);
                slot.publish(pane, generation, current.clone(), app_tx);
            }
            Ok(ServerMessage::Notify(message)) => {
                if effect_leader.load(Ordering::Acquire) {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::Notify(message),
                    });
                }
            }
            Ok(ServerMessage::Sound(signal)) => {
                if effect_leader.load(Ordering::Acquire) {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::Sound(signal),
                    });
                }
            }
            Ok(ServerMessage::Clipboard(text)) => {
                if effect_leader.load(Ordering::Acquire) {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::Clipboard(text),
                    });
                }
            }
            Ok(ServerMessage::OpenUrl(url)) => {
                if effect_leader.load(Ordering::Acquire) {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::OpenUrl(url),
                    });
                }
            }
            Ok(ServerMessage::Detach | ServerMessage::ServerShutdown { .. }) => break,
            Ok(_) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    fn frame(symbol: &str) -> FrameData {
        FrameData {
            width: 1,
            height: 1,
            cells: vec![protocol::CellData {
                symbol: symbol.into(),
                fg: 0,
                bg: 0,
                mods: 0,
            }],
            cursor: None,
            cursor_visible: false,
        }
    }

    #[test]
    fn merged_agents_dock_keeps_remote_owner_status() {
        let _env = crate::persist::test_env("merged-agent-status");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let (_pane, _input, _) = add_remote_workspace(&mut app);
        let response = json!({"result": {"event_sequence": 12, "workspaces": [{
            "id": "workspace_remote", "name": "remote-api", "cwd": "/srv/api",
            "tabs": [{"index": 2, "panes": [{"pane_id": "7", "kind": "terminal",
                "agent": "codex", "is_agent": true, "agent_status": "blocked", "focused": true}]}]
        }]}});
        let snapshot = parse_remote_snapshot(&response, RemoteBinaryLocation::Path).unwrap();
        app.apply_remote_session_discovered(
            RemoteSession::new("dev-207", "api").unwrap(),
            Ok(snapshot),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("codex"),
            "remote agent disappeared from Agents dock: {text}"
        );
    }

    fn add_remote_workspace(app: &mut App) -> (PaneId, mpsc::Receiver<ClientMessage>, PathBuf) {
        let pane = PaneId::alloc();
        let (input, receiver) = mpsc::channel();
        let target = RemoteWorkspaceRef {
            host: "dev-207".into(),
            session: "api".into(),
            workspace_id: "workspace_remote".into(),
        };
        let remote_path = PathBuf::from("/srv/api");
        app.views.insert(
            pane,
            ViewKind::Remote(RemoteView {
                agents: Vec::new(),
                target: target.clone(),
                state: RemoteViewState::Ready,
                error: None,
                frame: Some(frame("r")),
                generation: 1,
                input: Some(input),
                last_size: (80, 24),
                effect_leader: Arc::new(AtomicBool::new(true)),
            }),
        );
        app.workspaces.push(Workspace {
            id: "workspace_local_projection".into(),
            name: "api".into(),
            cwd: remote_path.clone(),
            branch: Some("main".into()),
            git_ahead_behind: None,
            worktree: None,
            tabs: vec![Tab::panes(TileLayout::new(pane))],
            active_tab: 0,
            pinned: false,
            remote: Some(target),
        });
        app.active_ws = app.workspaces.len() - 1;
        (pane, receiver, remote_path)
    }

    #[test]
    fn remote_commands_use_semantics_and_local_session_menu_stays_local() {
        let _env = crate::persist::test_env("remote-semantic-shortcuts");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        app.server_mode = true;
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        // A local prefix command must not synthesize that prefix remotely:
        // the owner may have an entirely different prefix/key map.
        app.mode = Mode::Prefix;
        let key = app
            .keymap
            .iter()
            .find(|(_, cmd)| **cmd == Cmd::NewTab)
            .unwrap()
            .0;
        let key = KeyEvent::new(
            KeyCode::Char(key.chars().next().unwrap()),
            KeyModifiers::NONE,
        );
        app.handle_active_remote_key(key);
        assert!(
            matches!(receiver.recv().unwrap(), ClientMessage::Command(command) if command == "new_tab")
        );
        app.mode = Mode::Prefix;
        let key = app
            .keymap
            .iter()
            .find(|(_, cmd)| **cmd == Cmd::OpenSessions)
            .unwrap()
            .0;
        let key = KeyEvent::new(
            KeyCode::Char(key.chars().next().unwrap()),
            KeyModifiers::NONE,
        );
        app.handle_active_remote_key(key);
        assert!(app.named_session_menu.is_some());
        assert!(
            receiver.try_recv().is_err(),
            "session selector belongs to the local UI"
        );
        app.named_session_menu = None;
        let image = crate::terminal::clipboard::ClipboardImage {
            extension: "png".into(),
            bytes: b"\x89PNG\r\n\x1a\ntest".to_vec(),
        };
        app.handle_event(AppEvent::ClipboardImage(image));
        assert!(
            matches!(receiver.recv().unwrap(), ClientMessage::ClipboardImage(image) if image.valid())
        );
    }

    #[test]
    fn remote_frame_slot_coalesces_to_the_newest_complete_frame() {
        let (tx, rx) = mpsc::channel();
        let slot = Arc::new(RemoteFrameSlot::new());
        let pane = PaneId::alloc();
        slot.publish(pane, 1, frame("a"), &tx);
        slot.publish(pane, 1, frame("b"), &tx);
        assert!(matches!(
            rx.recv().unwrap(),
            AppEvent::RemoteFrameAvailable { pane: event_pane, generation: 1, .. }
                if event_pane == pane
        ));
        assert!(rx.try_recv().is_err());
        assert!(slot.take().is_some_and(|next| next == frame("b")));

        slot.publish(pane, 1, frame("c"), &tx);
        assert!(rx.recv().is_ok());
        assert!(slot.take().is_some_and(|next| next == frame("c")));
    }

    #[test]
    fn remote_key_paste_and_mouse_are_forwarded_in_projection_coordinates() {
        let _env = crate::persist::test_env("remote-view-input-forwarding");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (pane, receiver, _) = add_remote_workspace(&mut app);
        app.pane_content_rects = vec![(pane, Rect::new(10, 5, 30, 12))];

        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.handle_active_remote_key(key), Some(false));
        assert!(matches!(receiver.recv().unwrap(), ClientMessage::Key(sent) if sent == key));

        assert!(app.send_active_remote(ClientMessage::Paste("hello".into())));
        assert!(matches!(receiver.recv().unwrap(), ClientMessage::Paste(text) if text == "hello"));

        let mouse = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 14,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.forward_active_remote_mouse(mouse));
        assert!(matches!(
            receiver.recv().unwrap(),
            ClientMessage::Mouse(MouseEvent {
                column: 4,
                row: 3,
                ..
            })
        ));
    }

    #[test]
    fn remote_workspaces_are_not_persisted_or_recorded_as_closed_local_paths() {
        let _env = crate::persist::test_env("remote-view-persistence-boundary");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (_pane, receiver, remote_path) = add_remote_workspace(&mut app);
        let snapshot = crate::persist::snapshot(&app);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert!(snapshot
            .workspaces
            .iter()
            .all(|workspace| workspace.cwd != remote_path));

        let remote_index = app.active_ws;
        app.close_workspace(remote_index);
        assert!(!app.closed_workspace_paths.contains(&remote_path));
        assert!(matches!(receiver.recv().unwrap(), ClientMessage::Detach));
        app.apply_remote_session_discovered(
            RemoteSession::new("dev-207", "api").unwrap(),
            Ok(RemoteSessionSnapshot {
                location: RemoteBinaryLocation::Path,
                event_sequence: 2,
                workspaces: vec![RemoteWorkspaceMeta {
                    agents: Vec::new(),
                    worktree: None,
                    id: "workspace_remote".into(),
                    name: "api".into(),
                    cwd: remote_path.display().to_string(),
                    branch: None,
                }],
            }),
        );
        assert!(app
            .workspaces
            .iter()
            .all(|workspace| workspace.remote.is_none()));
    }

    #[test]
    fn topology_snapshots_update_and_remove_existing_remote_workspaces() {
        let _env = crate::persist::test_env("remote-view-topology-reconcile");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        let target = RemoteSession::new("dev-207", "api").unwrap();

        app.apply_remote_session_discovered(
            target.clone(),
            Ok(RemoteSessionSnapshot {
                location: RemoteBinaryLocation::Path,
                event_sequence: 4,
                workspaces: vec![RemoteWorkspaceMeta {
                    agents: Vec::new(),
                    worktree: None,
                    id: "workspace_remote".into(),
                    name: "renamed-api".into(),
                    cwd: "/srv/api-v2".into(),
                    branch: Some("release".into()),
                }],
            }),
        );
        let workspace = app
            .workspaces
            .iter()
            .find(|workspace| workspace.remote.is_some())
            .unwrap();
        assert_eq!(workspace.name, "renamed-api");
        assert_eq!(workspace.cwd, PathBuf::from("/srv/api-v2"));
        assert_eq!(workspace.branch.as_deref(), Some("release"));

        app.apply_remote_session_discovered(
            target,
            Ok(RemoteSessionSnapshot {
                location: RemoteBinaryLocation::Path,
                event_sequence: 5,
                workspaces: Vec::new(),
            }),
        );
        assert!(app
            .workspaces
            .iter()
            .all(|workspace| workspace.remote.is_none()));
        assert!(matches!(receiver.recv().unwrap(), ClientMessage::Detach));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_remote_refresh_reconnects_same_workspace_and_fences_previous_bridge() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePath(Option<std::ffi::OsString>);
        impl Drop for RestorePath {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(path) => std::env::set_var("PATH", path),
                    None => std::env::remove_var("PATH"),
                }
            }
        }

        let _env = crate::persist::test_env("remote-explicit-reconnect");
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (pane, _receiver, _) = add_remote_workspace(&mut app);
        let target = RemoteSession::new("dev-207", "api").unwrap();
        let helper_dir = crate::persist::config_dir().join("ssh-fixture");
        std::fs::create_dir_all(&helper_dir).unwrap();
        let helper = helper_dir.join("ssh");
        std::fs::write(&helper, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _path = RestorePath(std::env::var_os("PATH"));
        std::env::set_var("PATH", &helper_dir);

        app.apply_remote_projection_closed(pane, 1, "bridge closed".into());
        app.remote_session_watchers.insert(
            target.canonical_name(),
            RemoteWatcher {
                generation: 5,
                scope: Arc::new(Default::default()),
                refresh_projections: true,
            },
        );
        let snapshot = RemoteSessionSnapshot {
            location: RemoteBinaryLocation::Path,
            event_sequence: 9,
            workspaces: vec![RemoteWorkspaceMeta {
                agents: Vec::new(),
                worktree: None,
                id: "workspace_remote".into(),
                name: "api".into(),
                cwd: "/srv/api".into(),
                branch: None,
            }],
        };
        app.apply_remote_session_discovered(target.clone(), Ok(snapshot.clone()));
        let view = app.remote_workspace_view(app.active_ws).unwrap();
        assert_eq!(view.state, RemoteViewState::Connecting);
        assert_eq!(view.generation, 2);
        assert_eq!(
            app.workspaces[app.active_ws].id,
            "workspace_local_projection"
        );
        assert_eq!(app.workspaces.len(), 2);

        app.apply_remote_projection_closed(pane, 1, "late previous bridge".into());
        assert_eq!(
            app.remote_workspace_view(app.active_ws).unwrap().state,
            RemoteViewState::Connecting
        );
        // Wait for the fixture bridge to finish before restoring PATH. No SSH
        // command from this test can escape to the user's real SSH client.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            if let AppEvent::RemoteProjectionClosed {
                pane: closed,
                generation,
                error,
            } = event
            {
                assert_eq!(closed, pane);
                assert_eq!(generation, 2);
                app.apply_remote_projection_closed(closed, generation, error);
                break;
            }
        }

        // Ordinary owner events must not turn this explicit retry into an
        // automatic retry loop after another failure.
        app.apply_remote_session_discovered(target, Ok(snapshot));
        let view = app.remote_workspace_view(app.active_ws).unwrap();
        assert_eq!(view.state, RemoteViewState::Disconnected);
        assert_eq!(view.generation, 2);
    }

    #[test]
    fn explicit_remote_refresh_replaces_topology_watcher_only_for_disconnected_views() {
        let _env = crate::persist::test_env("remote-refresh-watcher");
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (pane, _receiver, _) = add_remote_workspace(&mut app);
        let target = RemoteSession::new("dev-207", "api").unwrap();
        app.config.remote_hosts.push(target.host.clone());
        app.remote_watcher_generation = 4;
        app.remote_session_watchers.insert(
            target.canonical_name(),
            RemoteWatcher {
                generation: 4,
                scope: Arc::new(Default::default()),
                refresh_projections: false,
            },
        );
        app.discover_remote_session(target.clone());
        assert!(app.remote_watcher_is_current(&target, 4));

        app.apply_remote_projection_closed(pane, 1, "display bridge failed".into());
        app.discover_remote_session(target.clone());
        assert!(!app.remote_watcher_is_current(&target, 4));
        assert!(app.remote_watcher_is_current(&target, 5));
        assert!(app.remote_session_watchers[&target.canonical_name()].refresh_projections);

        // The isolated on-disk configuration has no enabled hosts. The worker
        // must finish at that validation boundary without launching real SSH.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            if let AppEvent::RemoteSessionDiscovered {
                generation, result, ..
            } = event
            {
                assert_eq!(generation, 5);
                assert!(result.unwrap_err().contains("not enabled"));
                break;
            }
        }
    }

    #[test]
    fn remote_registry_handoff_requires_an_attached_client() {
        let _env = crate::persist::test_env("remote-merge-unattached-handoff");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let target = RemoteSession::new("dev-207", "stopped-local").unwrap();
        app.remote_merge_enabled = true;
        assert!(!app.has_attached_client);
        // A background proxy reloading global settings must not prepare/start
        // this same-name namespace. Otherwise restart --all starts stopped
        // sessions indirectly through its restored proxy servers.
        assert_eq!(app.remote_merge_switch_target(Some(&target)), None);

        app.has_attached_client = true;
        assert_eq!(
            app.remote_merge_switch_target(Some(&target)),
            Some("stopped-local".into())
        );
        assert_eq!(app.remote_merge_switch_target(None), None);
        app.remote_merge_enabled = false;
        assert_eq!(app.remote_merge_switch_target(Some(&target)), None);
    }

    #[test]
    fn remote_projection_ready_resends_size_recorded_before_handshake() {
        let _env = crate::persist::test_env("remote-ready-viewport");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (pane, _receiver, _) = add_remote_workspace(&mut app);
        let (input, messages) = mpsc::channel();
        if let Some(ViewKind::Remote(view)) = app.views.get_mut(&pane) {
            view.last_size = (91, 27);
        }
        app.pane_content_rects = vec![(pane, Rect::new(9, 2, 91, 27))];
        app.apply_remote_projection_ready(pane, 1, input);
        app.resize_active_remote_projection();
        assert!(matches!(
            messages.try_recv().unwrap(),
            ClientMessage::Resize { cols: 91, rows: 27 }
        ));
        app.resize_active_remote_projection();
        assert!(matches!(
            messages.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn effect_leadership_moves_to_a_connected_projection() {
        let _env = crate::persist::test_env("remote-view-effect-leader");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (first, _receiver, _) = add_remote_workspace(&mut app);
        let first_leader = match &app.views[&first] {
            ViewKind::Remote(view) => view.effect_leader.clone(),
            _ => unreachable!(),
        };
        let second = PaneId::alloc();
        let second_leader = Arc::new(AtomicBool::new(false));
        let (input, _receiver) = mpsc::channel();
        app.views.insert(
            second,
            ViewKind::Remote(RemoteView {
                agents: Vec::new(),
                target: RemoteWorkspaceRef {
                    host: "dev-207".into(),
                    session: "api".into(),
                    workspace_id: "workspace_remote_2".into(),
                },
                state: RemoteViewState::Ready,
                error: None,
                frame: Some(frame("s")),
                generation: 2,
                input: Some(input),
                last_size: (80, 24),
                effect_leader: second_leader.clone(),
            }),
        );

        assert!(first_leader.load(Ordering::Acquire));
        assert!(!second_leader.load(Ordering::Acquire));

        app.apply_remote_projection_closed(first, 1, "bridge closed".into());
        assert!(!first_leader.load(Ordering::Acquire));
        assert!(second_leader.load(Ordering::Acquire));
    }

    #[test]
    fn remote_snapshot_projects_only_workspaces_owned_by_that_host() {
        let response = json!({
            "id":"remote-session-discovery",
            "result":{
                "event_sequence":9,
                "workspaces":[
                    {
                        "id":"workspace_local",
                        "name":"api",
                        "cwd":"/srv/api",
                        "branch":"main",
                        "host":null
                    },
                    {
                        "id":"workspace_nested",
                        "name":"nested",
                        "cwd":"/srv/nested",
                        "branch":"dev",
                        "host":"another-host"
                    }
                ]
            }
        });
        let snapshot = parse_remote_snapshot(&response, RemoteBinaryLocation::Path).unwrap();
        assert_eq!(snapshot.event_sequence, 9);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].id, "workspace_local");
    }

    #[test]
    fn disabling_hosts_removes_projections_and_fences_old_discovery_events() {
        let _env = crate::persist::test_env("remote-disable-host");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        let target = RemoteSession::new("dev-207", "api").unwrap();
        app.remote_session_watchers.insert(
            target.canonical_name(),
            RemoteWatcher {
                generation: 5,
                scope: Arc::new(Default::default()),
                refresh_projections: true,
            },
        );
        app.config.remote_hosts.clear();
        app.start_merged_remote_sessions();
        assert!(app.remote_session_watchers.is_empty());
        assert!(app
            .workspaces
            .iter()
            .all(|workspace| workspace.remote.is_none()));
        assert!(matches!(receiver.recv().unwrap(), ClientMessage::Detach));
        app.handle_event(AppEvent::RemoteSessionDiscovered {
            target,
            generation: 5,
            result: Ok(RemoteSessionSnapshot {
                location: RemoteBinaryLocation::Path,
                event_sequence: 7,
                workspaces: vec![RemoteWorkspaceMeta {
                    agents: Vec::new(),
                    worktree: None,
                    id: "late-workspace".into(),
                    name: "late".into(),
                    cwd: "/remote".into(),
                    branch: None,
                }],
            }),
        });
        assert!(app
            .workspaces
            .iter()
            .all(|workspace| workspace.remote.is_none()));
    }

    #[test]
    fn cli_requires_the_owner_host_without_breaking_explicit_local_pane_targets() {
        let _env = crate::persist::test_env("remote-cli-owner-boundary");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let local_pane = app.layout().focus;
        let (remote_pane, _receiver, _) = add_remote_workspace(&mut app);
        let before = app.workspaces.len();
        for method in ["pane.split", "tab.new", "worktree.create", "files.tree"] {
            assert_eq!(
                app.dispatch(method, &json!({})).unwrap_err().0,
                "remote_workspace"
            );
        }
        assert_eq!(
            app.dispatch("pane.close", &json!({"pane":remote_pane.0.to_string()}))
                .unwrap_err()
                .0,
            "remote_workspace"
        );
        assert!(app
            .dispatch("pane.get", &json!({"pane":local_pane.0.to_string()}))
            .is_ok());
        assert_eq!(app.workspaces.len(), before);
    }
}
