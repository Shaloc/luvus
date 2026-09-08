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
use std::time::Duration;

use serde_json::{json, Value};

use super::{App, Cmd, Mode, Tab, ViewKind, Workspace};
use crate::event::AppEvent;
use crate::ids::PaneId;
use crate::ipc::protocol::{self, ClientMessage, FrameData, ServerMessage};
use crate::layout::TileLayout;
use crate::session::remote::{RemoteBinaryLocation, RemoteInput, RemoteSession};

const REMOTE_RETRY_INITIAL: Duration = Duration::from_millis(250);
const REMOTE_RETRY_MAX: Duration = Duration::from_secs(10);

pub(super) struct RemoteWatcher {
    target: RemoteSession,
    generation: u64,
    scope: Arc<crate::session::remote::ConnectionScope>,
    /// Reconcile projections once after explicit discovery or a failed connection.
    refresh_projections: bool,
    /// None until this owner has connected successfully. Failed initial opens
    /// still report their install/configuration error without an idle retry loop.
    retry_delay: Option<Duration>,
    retry_pending: bool,
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
    pub terminal_id: Option<String>,
    pub cwd: String,
    pub pinned: bool,
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
                    .get("workspace_focused")
                    .or_else(|| pane.get("focused"))
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
                terminal_id: pane
                    .get("terminal_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                cwd: pane
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                pinned: pane
                    .get("agent_pinned")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteWorkspaceMeta {
    pub agents: Vec<RemoteAgentMeta>,
    pub history: Vec<super::remote_agents::AgentHistoryRow>,
    pub scheduled: Vec<super::remote_agents::ScheduledAgentRow>,
    pub worktree: Option<crate::git::WorktreeMembership>,
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteSessionSnapshot {
    pub event_sequence: u64,
    pub workspaces: Vec<RemoteWorkspaceMeta>,
    pub display: RemoteDisplay,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteDisplay {
    pub location: RemoteBinaryLocation,
    pub server_generation: Option<String>,
    pub projection: bool,
}

#[derive(Default)]
pub struct RemoteProjection {
    display: RemoteDisplay,
    epoch: u64,
    active: bool,
    frame_state: Option<protocol::ProjectionState>,
    desired_pane: Option<String>,
    presented: Option<(u64, u64)>,
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
    Workspace {
        pane: PaneId,
        generation: u64,
        workspace_id: String,
    },
    Session {
        pane: PaneId,
        generation: u64,
        name: String,
    },
    Detach {
        pane: PaneId,
        generation: u64,
    },
}

/// One explicit owner-menu destination awaiting the existing topology feed.
/// Never persisted, retried on a timer, or allowed to outlive its input source.
pub(super) struct PendingRemoteNavigation {
    pane: PaneId,
    generation: u64,
    target: RemoteWorkspaceRef,
}

pub struct RemoteView {
    pub agents: Vec<RemoteAgentMeta>,
    pub history: Vec<super::remote_agents::AgentHistoryRow>,
    pub scheduled: Vec<super::remote_agents::ScheduledAgentRow>,
    pub target: RemoteWorkspaceRef,
    pub state: RemoteViewState,
    pub error: Option<String>,
    pub frame: Option<FrameData>,
    pub generation: u64,
    pub input: Option<RemoteInput>,
    pub last_size: (u16, u16),
    pub projection: Box<RemoteProjection>,
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
    latest: Mutex<Option<(FrameData, Option<protocol::ProjectionState>)>>,
    pending: AtomicBool,
}

/// Reap the SSH bridge on every return path, including handshake and frame
/// decode failures. The input writer owns only the child's stdin pipe, so
/// terminating the process also lets that short-lived thread exit promptly.
struct RemoteChild(Arc<Mutex<Child>>);

impl Drop for RemoteChild {
    fn drop(&mut self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
        self.publish_with_state(pane, generation, frame, None, tx);
    }

    fn publish_with_state(
        self: &Arc<Self>,
        pane: PaneId,
        generation: u64,
        frame: FrameData,
        state: Option<protocol::ProjectionState>,
        tx: &mpsc::Sender<AppEvent>,
    ) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some((frame, state));
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

    pub(crate) fn take(&self) -> Option<(FrameData, Option<protocol::ProjectionState>)> {
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
        // Choosing the current owner should browse from the workspace the
        // user is viewing, not jump to that owner's first projected checkout.
        let active = self.active_ws;
        let Some(index) = std::iter::once(active)
            .chain((0..self.workspaces.len()).filter(|index| *index != active))
            .find(|&index| {
                self.remote_workspace_view(index).is_some_and(|view| {
                    view.target.host == target.host
                        && view.target.session == target.session
                        && view.input.is_some()
                })
            })
        else {
            return false;
        };
        self.focus_workspace(index);
        if self.send_workspace_remote(index, ClientMessage::Command("open_local_workspace".into()))
        {
            self.picker = None;
            self.focus_workspace(index);
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
        self.focus_workspace(index);
        if menu {
            if let Some(pane) = pane.parse::<u32>().ok().map(PaneId) {
                let view = self.workspaces[index].tabs[0].layout.focus;
                self.open_agent_menu(super::AgentTarget::RemoteLive { view, pane }, 2, 2);
            }
        } else {
            self.sidebar_focus = None;
            if let Some(ViewKind::Remote(view)) = self
                .views
                .get_mut(&self.workspaces[index].tabs[0].layout.focus)
            {
                if view.projection.display.projection {
                    view.projection.desired_pane = Some(pane.to_string());
                }
            }
            self.send_workspace_remote(
                index,
                ClientMessage::Command(format!("remote_agent_focus {pane}")),
            );
        }
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
                history: Vec::new(),
                scheduled: Vec::new(),
                target: reference.clone(),
                state: RemoteViewState::Connecting,
                error: None,
                frame: None,
                generation: u64::from(pane.0),
                input: None,
                last_size: (0, 0),
                projection: Box::default(),
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
        // Revocation follows the in-memory selection immediately, including
        // while a save is pending or failing. Only new admission waits for disk.
        let enabled_hosts = self.config.remote_hosts.clone();
        self.retain_remote_sessions(|host, _| enabled_hosts.iter().any(|item| item == host));
        self.remote_host_status
            .retain(|status| enabled_hosts.contains(&status.host));
        // SSH admission and discovery read the saved configuration. Settings
        // now saves asynchronously; fence older results and defer discovery
        // until the existing save completion has persisted the latest choices.
        if self.config_save_pending() {
            self.remote_config_refresh_pending = true;
            return;
        }
        self.remote_config_refresh_pending = false;
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
                let result = (|| {
                    if !crate::session::owner_session_exists(&name)? {
                        return Ok(None);
                    }
                    crate::session::start_client_session(&name)?;
                    crate::session::remote::reload_local_session(&name)?;
                    Ok(Some(name))
                })();
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
        self.retain_remote_sessions(|host, session| {
            targets
                .iter()
                .any(|target| target.host == host && target.session == session)
        });
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

    fn retain_remote_sessions(&mut self, keep: impl Fn(&str, &str) -> bool) {
        self.remote_session_watchers
            .retain(|_, watcher| keep(&watcher.target.host, &watcher.target.session));
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
                (!keep(&remote.host, &remote.session)).then_some(index)
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
    }

    pub(super) fn finish_remote_config_refresh(&mut self) -> bool {
        if self.remote_config_refresh_pending && !self.config_save_pending() {
            self.start_merged_remote_sessions();
            true
        } else {
            false
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
        self.discover_remote_session_with_refresh(target, false);
    }

    fn discover_remote_session_with_refresh(
        &mut self,
        target: RemoteSession,
        force_snapshot: bool,
    ) {
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
        if self.remote_session_watchers.contains_key(&key) && !disconnected && !force_snapshot {
            return;
        }
        // User-triggered refresh also replaces a healthy topology watcher when
        // its independent display bridge failed. Dropping it cancels its I/O;
        // the new generation fences any snapshot already queued by that watcher.
        self.start_remote_watcher(target, None);
    }

    fn start_remote_watcher(&mut self, target: RemoteSession, retry: Option<Duration>) {
        let key = target.canonical_name();
        self.remote_session_watchers.remove(&key);
        // Replacing the session scope also closes its display bridges. Fence
        // their queued frames even when this is a healthy, explicit refresh.
        for view in self.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                if view.target.host == target.host && view.target.session == target.session {
                    if let Some(input) = view.input.take() {
                        let _ = input.send(ClientMessage::Detach);
                    }
                    view.generation = view.generation.wrapping_add(1);
                    view.state = RemoteViewState::Connecting;
                }
            }
        }
        self.discard_stale_remote_navigation();
        self.remote_watcher_generation = self.remote_watcher_generation.wrapping_add(1);
        let generation = self.remote_watcher_generation;
        let scope = Arc::new(crate::session::remote::ConnectionScope::default());
        self.remote_session_watchers.insert(
            key,
            RemoteWatcher {
                target: target.clone(),
                generation,
                scope: scope.clone(),
                refresh_projections: true,
                retry_delay: retry.map(|delay| (delay * 2).min(REMOTE_RETRY_MAX)),
                retry_pending: retry.is_some(),
            },
        );
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            if retry.is_some_and(|delay| !scope.wait_for_retry(delay)) {
                return;
            }
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
            let location = snapshot.display.location;
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

    /// One retry per selected session, shared by topology and display failures.
    /// A fresh scope cancels both old streams; the existing generations reject
    /// already queued frames/snapshots before any owner-local pane ID is reused.
    fn retry_remote_session(&mut self, target: &RemoteSession, error: &str) -> bool {
        if crate::session::remote::failure_needs_attention(error) {
            return false;
        }
        if !self.config.remote_hosts.contains(&target.host) {
            return false;
        }
        let Some(watcher) = self.remote_session_watchers.get(&target.canonical_name()) else {
            return false;
        };
        if watcher.retry_pending {
            return true;
        }
        let Some(delay) = watcher.retry_delay else {
            return false;
        };
        for view in self.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                if view.target.host == target.host && view.target.session == target.session {
                    view.error = Some(error.to_string());
                    for agent in &mut view.agents {
                        agent.state = crate::ui::theme::State::Unknown;
                    }
                }
            }
        }
        self.start_remote_watcher(target.clone(), Some(delay));
        self.rebalance_remote_effect_leaders();
        true
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
        self.discard_stale_remote_navigation();
        if let Some(watcher) = self
            .remote_session_watchers
            .get_mut(&target.canonical_name())
        {
            watcher.retry_pending = false;
        }
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if self.retry_remote_session(&target, &error) {
                    return;
                }
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
                self.discard_stale_remote_navigation();
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
            .is_some_and(|watcher| {
                watcher.retry_delay.get_or_insert(REMOTE_RETRY_INITIAL);
                std::mem::take(&mut watcher.refresh_projections)
            });
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
                history: Vec::new(),
                scheduled: Vec::new(),
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
                        // Focus belongs to the displayed frame. The topology
                        // stream can arrive before or after that frame.
                        if let Some(state) = &view.projection.frame_state {
                            for agent in &mut view.agents {
                                agent.focused = Some(&agent.pane) == state.focused_pane.as_ref();
                            }
                        }
                        view.history = meta.history;
                        view.scheduled = meta.scheduled;
                        if refresh_projections && view.state != RemoteViewState::Ready {
                            if let Some(input) = view.input.take() {
                                let _ = input.send(ClientMessage::Detach);
                            }
                            view.generation = view.generation.wrapping_add(1);
                            view.state = RemoteViewState::Connecting;
                            *view.projection = RemoteProjection {
                                display: snapshot.display.clone(),
                                ..Default::default()
                            };
                            view.error = None;
                            reconnect.push((
                                pane,
                                view.generation,
                                remote.clone(),
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
            self.add_remote_workspace(&target, meta, snapshot.display.clone());
        }
        for (pane, generation, remote, effect_leader) in reconnect {
            spawn_projection(
                pane,
                generation,
                remote,
                snapshot.display.clone(),
                effect_leader,
                self.app_tx.clone(),
                self.remote_connection_scope(&target),
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
        self.finish_remote_navigation();
    }

    pub(crate) fn apply_remote_session_watcher_closed(
        &mut self,
        target: RemoteSession,
        error: String,
    ) {
        if self.retry_remote_session(&target, &error) {
            return;
        }
        if self
            .pending_remote_navigation
            .as_ref()
            .is_some_and(|pending| {
                pending.target.host == target.host && pending.target.session == target.session
            })
        {
            self.pending_remote_navigation = None;
        }
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
        display: RemoteDisplay,
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
                history: meta.history,
                scheduled: meta.scheduled,
                target: remote.clone(),
                state: RemoteViewState::Connecting,
                error: None,
                frame: None,
                generation,
                input: None,
                last_size: (80, 24),
                projection: Box::new(RemoteProjection {
                    display: display.clone(),
                    ..Default::default()
                }),
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
            remote,
            display,
            effect_leader,
            self.app_tx.clone(),
            self.remote_connection_scope(target),
        );
    }

    fn remote_connection_scope(
        &self,
        target: &RemoteSession,
    ) -> Arc<crate::session::remote::ConnectionScope> {
        self.remote_session_watchers
            .get(&target.canonical_name())
            .map(|watcher| watcher.scope.clone())
            .unwrap_or_default()
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
        input: RemoteInput,
    ) {
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if view.generation != generation {
            return;
        }
        view.input = Some(input);
        view.state = if view.projection.display.projection {
            RemoteViewState::Connecting
        } else {
            RemoteViewState::Ready
        };
        view.error = None;
        // The new owner connection starts at its handshake viewport. A render
        // while connecting may already have cached our desired size without
        // an input sender, so force it to be sent after this handshake.
        view.last_size = (0, 0);
        let target = RemoteSession {
            host: view.target.host.clone(),
            session: view.target.session.clone(),
        };
        if self.views.values().all(|view| {
            !matches!(view, ViewKind::Remote(view)
            if view.target.host == target.host && view.target.session == target.session
                && view.input.is_none())
        }) {
            if let Some(watcher) = self
                .remote_session_watchers
                .get_mut(&target.canonical_name())
            {
                watcher.retry_delay = Some(REMOTE_RETRY_INITIAL);
            }
        }
        self.rebalance_remote_effect_leaders();
    }

    pub(crate) fn apply_remote_frame(
        &mut self,
        pane: PaneId,
        generation: u64,
        slot: &RemoteFrameSlot,
    ) {
        let Some((frame, state)) = slot.take() else {
            return;
        };
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if view.generation == generation {
            if view.projection.display.projection {
                let Some(state) = state else {
                    return;
                };
                if !view.projection.active
                    || state.epoch != view.projection.epoch
                    || Some(&state.server_generation)
                        != view.projection.display.server_generation.as_ref()
                    || state.workspace_id != view.target.workspace_id
                    || (frame.width, frame.height) != view.last_size
                    || view
                        .projection
                        .desired_pane
                        .as_ref()
                        .is_some_and(|pane| Some(pane) != state.focused_pane.as_ref())
                {
                    return;
                }
                for agent in &mut view.agents {
                    agent.focused = Some(&agent.pane) == state.focused_pane.as_ref();
                }
                view.projection.desired_pane = None;
                view.projection.frame_state = Some(state);
            }
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
        if view.generation != generation {
            return;
        }
        view.state = RemoteViewState::Disconnected;
        view.input = None;
        view.error = Some(error.clone());
        let target = RemoteSession {
            host: view.target.host.clone(),
            session: view.target.session.clone(),
        };
        self.retry_remote_session(&target, &error);
        self.discard_stale_remote_navigation();
        self.rebalance_remote_effect_leaders();
    }

    pub(crate) fn apply_remote_effect(&mut self, effect: RemoteEffect) {
        match effect {
            RemoteEffect::Notify(message) => self.pending_notify.push(message),
            RemoteEffect::Sound(signal) => self.pending_sound = Some(signal),
            RemoteEffect::Clipboard(text) => self.pending_clipboard = Some(text),
            RemoteEffect::OpenUrl(url) => self.pending_open_url = Some(url),
            RemoteEffect::Workspace {
                pane,
                generation,
                workspace_id,
            } => {
                if let Some(target) =
                    self.apply_remote_workspace_navigation(pane, generation, workspace_id)
                {
                    // Closed projections have no retained metadata, and merely
                    // focusing an existing owner workspace need not emit a new
                    // topology event. Reuse one explicit discovery, replacing
                    // its old watcher instead of waiting indefinitely or polling.
                    self.discover_remote_session_with_refresh(target, true);
                }
            }
            RemoteEffect::Session {
                pane,
                generation,
                name,
            } => {
                if let Some(target) = self.remote_session_navigation_target(pane, generation, &name)
                {
                    self.pending_remote_navigation = None;
                    self.open_named_session_menu();
                    self.prepare_remote_session(target, self.remote_merge_enabled, false);
                }
            }
            RemoteEffect::Detach { pane, generation } => {
                if self.remote_navigation_source(pane, generation).is_some() {
                    self.pending_remote_navigation = None;
                    self.detach_requested = true;
                }
            }
        }
    }

    fn remote_navigation_source(
        &self,
        pane: PaneId,
        generation: u64,
    ) -> Option<&RemoteWorkspaceRef> {
        if self.active_remote_pane() != Some(pane) {
            return None;
        }
        match self.views.get(&pane)? {
            ViewKind::Remote(view)
                if view.generation == generation && view.state == RemoteViewState::Ready =>
            {
                Some(&view.target)
            }
            _ => None,
        }
    }

    fn remote_session_navigation_target(
        &self,
        pane: PaneId,
        generation: u64,
        name: &str,
    ) -> Option<RemoteSession> {
        let source = self.remote_navigation_source(pane, generation)?;
        // This is an actual name on the current SSH owner, not a canonical
        // local registry selector and never a request for a second SSH hop.
        RemoteSession::new(&source.host, name).ok()
    }

    pub(crate) fn discard_stale_remote_navigation(&mut self) {
        if self
            .pending_remote_navigation
            .as_ref()
            .is_some_and(|pending| {
                self.remote_navigation_source(pending.pane, pending.generation)
                    .is_none()
            })
        {
            self.pending_remote_navigation = None;
        }
    }

    /// Return the owner needing an explicit snapshot only when no matching
    /// projection exists. Keeping the decision separate also lets native-only
    /// tests exercise the entire state transition without opening SSH.
    fn apply_remote_workspace_navigation(
        &mut self,
        pane: PaneId,
        generation: u64,
        workspace_id: String,
    ) -> Option<RemoteSession> {
        let source = self.remote_navigation_source(pane, generation)?;
        let owner = RemoteSession::new(&source.host, &source.session).ok()?;
        let target = RemoteWorkspaceRef {
            host: source.host.clone(),
            session: source.session.clone(),
            workspace_id,
        };
        // An explicit owner-menu choice may reopen that one locally hidden
        // projection, but stale or background messages have no such authority.
        self.closed_remote_workspaces.remove(&target);
        self.pending_remote_navigation = Some(PendingRemoteNavigation {
            pane,
            generation,
            target,
        });
        if self.finish_remote_navigation() {
            None
        } else {
            Some(owner)
        }
    }

    fn finish_remote_navigation(&mut self) -> bool {
        self.discard_stale_remote_navigation();
        let Some(pending) = self.pending_remote_navigation.as_ref() else {
            return false;
        };
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.remote.as_ref() == Some(&pending.target))
        else {
            return false;
        };
        self.pending_remote_navigation = None;
        self.focus_workspace(index);
        true
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
                ViewKind::Remote(view) if view.state == RemoteViewState::Ready => {
                    view.input.as_ref()
                }
                _ => None,
            })
            .is_some_and(|input| input.send(message).is_ok())
    }

    pub(crate) fn send_workspace_remote(&mut self, index: usize, message: ClientMessage) -> bool {
        if index == self.active_ws
            && self
                .remote_workspace_view(index)
                .is_some_and(|view| view.projection.display.projection)
        {
            self.resize_active_remote_projection();
        }
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

    /// Map an outer menu's click into the selected owner's display viewport.
    /// Sidebar anchors sit outside that viewport and clamp to its nearest edge.
    pub(crate) fn remote_menu_anchor(&self, workspace: usize, anchor: (u16, u16)) -> (u16, u16) {
        let rect = self.remote_workspace_rect(workspace);
        (
            anchor
                .0
                .saturating_sub(rect.x)
                .min(rect.width.saturating_sub(1)),
            anchor
                .1
                .saturating_sub(rect.y)
                .min(rect.height.saturating_sub(1)),
        )
    }

    pub(crate) fn remote_sidebar_reopen_height(&self) -> u16 {
        u16::from(
            [&self.sidebars.left, &self.sidebars.right]
                .iter()
                .any(|side| !side.visible && !side.docks.is_empty()),
        )
    }

    fn remote_workspace_rect(&self, workspace: usize) -> ratatui::layout::Rect {
        self.workspaces
            .get(workspace)
            .filter(|workspace| workspace.remote.is_some())
            .and_then(|workspace| workspace.tabs.get(workspace.active_tab))
            .and_then(|tab| {
                self.pane_content_rects
                    .iter()
                    .find(|(pane, _)| *pane == tab.layout.focus)
            })
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| {
                // A context menu may target an inactive workspace. Its next
                // projection fills the content between the already-rendered
                // outer sidebars; unlike local tabs it has no extra tab row.
                let main = self.last_main_area;
                let left = self.left_seam.map_or(main.x, |seam| seam.right());
                let right = self.right_seam.map_or(main.right(), |seam| seam.x);
                let navigation = self.remote_sidebar_reopen_height().min(main.height);
                ratatui::layout::Rect::new(
                    left,
                    main.y + navigation,
                    right.saturating_sub(left),
                    main.height - navigation,
                )
            })
    }

    pub(crate) fn owner_menu_coordinates<'a>(
        mut args: impl Iterator<Item = &'a str>,
    ) -> Option<(u16, u16)> {
        match (args.next(), args.next(), args.next()) {
            (None, None, None) => Some((2, 2)),
            (Some(column), Some(row), None) => Some((column.parse().ok()?, row.parse().ok()?)),
            _ => None,
        }
    }

    pub(crate) fn handle_active_remote_key(
        &mut self,
        key: ratatui::crossterm::event::KeyEvent,
    ) -> Option<bool> {
        self.active_remote_pane()?;
        if self.mode == Mode::Normal {
            // Outer list navigation owns ordinary keys even while its selected
            // workspace is remote. Explicit direct shortcuts still use the
            // existing owner-aware command route; the list handles its prefix.
            if self.sidebar_focus.is_some()
                && (self.prefix.matches(&key)
                    || super::keys::direct_command(&self.direct_keymap, &key).is_none())
            {
                return None;
            }
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
            if self.prefix.matches(&key) {
                // The owner's prefix may differ from this display's alias.
                // Ask its normal double-prefix handler to send exactly once.
                let _ = self.send_active_remote(ClientMessage::Command("send_prefix".into()));
                return Some(true);
            }
            let command = super::keys::key_string(&key)
                .and_then(|binding| self.keymap.get(&binding).copied());
            if command.is_some_and(outer_command) {
                self.run_cmd(command.expect("checked above"));
            } else {
                // Fixed keys (?, digits, scrollback) and custom bindings all
                // belong to the owner; do not duplicate its prefix dispatch.
                let _ = self.send_active_remote(ClientMessage::PrefixKey(key));
            }
            return Some(true);
        }
        Some(false)
    }

    pub(crate) fn forward_active_remote_mouse(
        &mut self,
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
        let sent = self.send_active_remote(ClientMessage::Mouse(mouse));
        if sent {
            if let ratatui::crossterm::event::MouseEventKind::Down(button) = mouse.kind {
                self.remote_mouse_capture = Some((pane, rect, button));
            }
        }
        sent
    }

    pub(crate) fn forward_captured_remote_mouse(
        &mut self,
        mut mouse: ratatui::crossterm::event::MouseEvent,
    ) -> bool {
        use ratatui::crossterm::event::MouseEventKind;
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.remote_mouse_capture = None;
            return false;
        }
        let Some((pane, previous_rect, pressed)) = self.remote_mouse_capture else {
            return false;
        };
        let button = match mouse.kind {
            MouseEventKind::Drag(button) | MouseEventKind::Up(button) => button,
            _ => return false,
        };
        if button != pressed {
            return false;
        }
        if matches!(mouse.kind, MouseEventKind::Up(_)) {
            self.remote_mouse_capture = None;
        }
        let rect = self
            .pane_content_rects
            .iter()
            .find_map(|(candidate, rect)| (*candidate == pane).then_some(*rect))
            .unwrap_or(previous_rect);
        mouse.column = mouse
            .column
            .saturating_sub(rect.x)
            .min(rect.width.saturating_sub(1));
        mouse.row = mouse
            .row
            .saturating_sub(rect.y)
            .min(rect.height.saturating_sub(1));
        if let Some(ViewKind::Remote(view)) = self.views.get(&pane) {
            if let Some(input) = &view.input {
                let _ = input.send(ClientMessage::Mouse(mouse));
            }
        }
        // The gesture still belongs to its original owner if it disconnected;
        // never reinterpret the release as a local selection/resize action.
        true
    }

    pub(crate) fn resize_active_remote_projection(&mut self) {
        let active = self.active_remote_pane();
        if self.remote_display_pane != active {
            if let Some(previous) = self.remote_display_pane {
                if let Some(ViewKind::Remote(view)) = self.views.get_mut(&previous) {
                    if view.projection.display.projection && view.projection.active {
                        view.projection.active = false;
                        view.projection.epoch = view.projection.epoch.saturating_add(1);
                        view.projection.frame_state = None;
                        if let Some(input) = &view.input {
                            let _ = input.send(ClientMessage::ProjectionInterest {
                                epoch: view.projection.epoch,
                                active: false,
                                cols: view.last_size.0,
                                rows: view.last_size.1,
                            });
                        }
                    }
                }
            }
            self.remote_display_pane = active;
        }
        let Some(pane) = active else {
            return;
        };
        let rect = self.remote_workspace_rect(self.active_ws);
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        let size = (rect.width.max(1), rect.height.max(1));
        if view.projection.display.projection {
            if let Some(input) = &view.input {
                if !view.projection.active || size != view.last_size {
                    view.projection.active = true;
                    view.projection.epoch = view.projection.epoch.saturating_add(1);
                    view.projection.frame_state = None;
                    view.state = RemoteViewState::Connecting;
                    view.last_size = size;
                    let _ = input.send(ClientMessage::ProjectionInterest {
                        epoch: view.projection.epoch,
                        active: true,
                        cols: size.0,
                        rows: size.1,
                    });
                }
            }
            return;
        }
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

    /// Called only by the interactive display render, never by command routing
    /// or metadata discovery. An activation request is not evidence of viewing.
    pub(crate) fn acknowledge_presented_remote_projection(&mut self) {
        let Some(pane) = self.active_remote_pane() else {
            return;
        };
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if !view.projection.active || view.state != RemoteViewState::Ready {
            return;
        }
        if let (Some(input), Some(state)) = (&view.input, &view.projection.frame_state) {
            let token = (state.epoch, state.event_sequence);
            if view.projection.presented != Some(token)
                && input
                    .send(ClientMessage::ProjectionPresented {
                        epoch: token.0,
                        event_sequence: token.1,
                    })
                    .is_ok()
            {
                view.projection.presented = Some(token);
            }
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
            | Cmd::FocusWorkspaces
            | Cmd::ToggleAgents
            | Cmd::ToggleAgentScope
            | Cmd::NextAttention
            | Cmd::Switcher
            | Cmd::GlobalSearch
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

pub(super) fn parse_remote_snapshot(
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
                history: serde_json::from_value(
                    workspace
                        .get("agent_history")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                )
                .map_err(|error| error.to_string())?,
                scheduled: serde_json::from_value(
                    workspace
                        .get("scheduled_agents")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                )
                .map_err(|error| error.to_string())?,
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
    let snapshot = RemoteSessionSnapshot {
        display: RemoteDisplay {
            location,
            server_generation: response
                .get("result")
                .and_then(|result| result.get("server_generation"))
                .and_then(Value::as_str)
                .map(str::to_string),
            projection: response
                .get("result")
                .and_then(|result| result.get("remote_display"))
                .is_some_and(|display| {
                    display.get("transport").and_then(Value::as_u64)
                        == Some(u64::from(protocol::PROTOCOL_VERSION))
                        && display
                            .get("capabilities")
                            .and_then(Value::as_array)
                            .is_some_and(|caps| {
                                caps.iter().any(|cap| {
                                    cap.as_str() == Some(protocol::PROJECTION_CAPABILITY)
                                })
                            })
                }),
        },
        event_sequence,
        workspaces,
    };
    if snapshot.display.projection
        && snapshot
            .display
            .server_generation
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err("remote projection protocol mismatch: missing server generation".into());
    }
    Ok(snapshot)
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
                    | "agent.history_changed"
                    | "agent.pin_changed"
                    | "automation.created"
                    | "automation.updated"
                    | "automation.rebound"
                    | "automation.enabled"
                    | "automation.disabled"
                    | "automation.deleted"
                    | "automation.run_queued"
                    | "automation.run_materialized"
                    | "automation.run_started"
                    | "automation.run_finished"
                    | "automation.run_failed"
                    | "automation.run_updated"
                    | "task.updated"
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
    target: RemoteWorkspaceRef,
    display: RemoteDisplay,
    effect_leader: Arc<AtomicBool>,
    app_tx: mpsc::Sender<AppEvent>,
    scope: Arc<crate::session::remote::ConnectionScope>,
) {
    std::thread::spawn(move || {
        let result = run_projection(
            pane,
            generation,
            &target,
            display,
            effect_leader,
            &app_tx,
            &scope,
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
    target: &RemoteWorkspaceRef,
    display: RemoteDisplay,
    effect_leader: Arc<AtomicBool>,
    app_tx: &mpsc::Sender<AppEvent>,
    scope: &crate::session::remote::ConnectionScope,
) -> Result<(), String> {
    let owner = RemoteSession {
        host: target.host.clone(),
        session: target.session.clone(),
    };
    let mut command =
        crate::session::remote::bridge_command(&owner, "remote-client-bridge", display.location);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = RemoteChild(Arc::new(Mutex::new(
        command.spawn().map_err(|error| error.to_string())?,
    )));
    scope.register(&child.0)?;
    let (mut input, output, stderr) = {
        let mut process = child
            .0
            .lock()
            .map_err(|_| "SSH bridge closed".to_string())?;
        (
            process
                .stdin
                .take()
                .ok_or_else(|| "SSH bridge has no stdin".to_string())?,
            process
                .stdout
                .take()
                .ok_or_else(|| "SSH bridge has no stdout".to_string())?,
            process
                .stderr
                .take()
                .ok_or_else(|| "SSH bridge has no stderr".to_string())?,
        )
    };
    let diagnostics = crate::session::remote::BridgeDiagnostics::capture(stderr);
    let run = || -> Result<(), String> {
        protocol::write_message(
            &mut input,
            &if display.projection {
                ClientMessage::HelloProjection {
                    version: protocol::PROTOCOL_VERSION,
                    workspace_id: target.workspace_id.clone(),
                }
            } else if target.workspace_id.is_empty() {
                ClientMessage::Hello {
                    version: protocol::LEGACY_PROTOCOL_VERSION,
                    cols: 80,
                    rows: 24,
                }
            } else {
                ClientMessage::HelloWorkspace {
                    version: protocol::LEGACY_PROTOCOL_VERSION,
                    cols: 80,
                    rows: 24,
                    workspace_id: target.workspace_id.clone(),
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

        let failed = app_tx.clone();
        let input_tx = RemoteInput::spawn(input, child.0.clone(), move |error| {
            let _ = failed.send(AppEvent::RemoteProjectionClosed {
                pane,
                generation,
                error,
            });
        });
        app_tx
            .send(AppEvent::RemoteProjectionReady {
                pane,
                generation,
                input: input_tx,
            })
            .map_err(|_| "local session closed".to_string())?;

        let slot = Arc::new(RemoteFrameSlot::new());
        let mut frame = None;
        loop {
            match protocol::read_message::<_, ServerMessage>(&mut output) {
                Ok(ServerMessage::ProjectionFrame { state, frame: next }) => {
                    if !display.projection {
                        return Err("unexpected remote projection codec".into());
                    }
                    frame = Some(next.clone());
                    slot.publish_with_state(pane, generation, next, Some(state), app_tx);
                }
                Ok(ServerMessage::ProjectionDiff { state, frame: diff }) => {
                    if !display.projection {
                        return Err("unexpected remote projection codec".into());
                    }
                    let Some(current) = frame.as_mut() else {
                        return Err("remote projection sent a diff before its full frame".into());
                    };
                    if current.width != diff.width || current.height != diff.height {
                        return Err("remote projection diff size mismatch".into());
                    }
                    protocol::apply_diff(current, &diff);
                    slot.publish_with_state(pane, generation, current.clone(), Some(state), app_tx);
                }
                Ok(ServerMessage::Frame(next)) => {
                    frame = Some(next.clone());
                    slot.publish(pane, generation, next, app_tx);
                }
                Ok(ServerMessage::FrameDiff(diff)) => {
                    let Some(current) = frame.as_mut() else {
                        return Err(
                            "remote projection sent a diff before its full frame".to_string()
                        );
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
                Ok(ServerMessage::FocusWorkspace { workspace_id }) => {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::Workspace {
                            pane,
                            generation,
                            workspace_id,
                        },
                    });
                }
                Ok(ServerMessage::SwitchSession { name }) => {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::Session {
                            pane,
                            generation,
                            name,
                        },
                    });
                }
                Ok(ServerMessage::Detach) => {
                    let _ = app_tx.send(AppEvent::RemoteEffect {
                        effect: RemoteEffect::Detach { pane, generation },
                    });
                    break;
                }
                Ok(ServerMessage::ServerShutdown { .. }) => break,
                Ok(_) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    };
    run().map_err(|error| diagnostics.failure(error))
}

#[cfg(test)]
pub(crate) mod tests {
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
    fn negotiated_remote_frame_rejects_old_boot_selection_focus_and_geometry() {
        let _env = crate::persist::test_env("remote-coherent-frame");
        let mut app = remote_ui_app();
        let (pane, receiver, _) = add_remote_workspace(&mut app);
        app.pane_content_rects = vec![(pane, Rect::new(30, 2, 1, 1))];
        let ViewKind::Remote(view) = app.views.get_mut(&pane).unwrap() else {
            unreachable!()
        };
        view.projection.display.projection = true;
        view.projection.display.server_generation = Some("boot-b".into());
        view.projection.desired_pane = Some("7".into());
        app.resize_active_remote_projection();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ClientMessage::ProjectionInterest {
                epoch: 1,
                active: true,
                ..
            }
        ));
        let state = protocol::ProjectionState {
            server_generation: "boot-b".into(),
            epoch: 1,
            event_sequence: 42,
            workspace_id: "workspace_remote".into(),
            focused_pane: Some("7".into()),
        };
        let (tx, _rx) = mpsc::channel();
        let slot = Arc::new(RemoteFrameSlot::new());
        for invalid in 0..6 {
            let mut stale = state.clone();
            let mut stale_frame = frame("stale");
            let mut generation = 1;
            match invalid {
                0 => stale.server_generation = "boot-a".into(),
                1 => stale.epoch = 0,
                2 => stale.workspace_id = "another-workspace".into(),
                3 => stale.focused_pane = Some("8".into()),
                4 => stale_frame.width = 2,
                _ => generation = 0,
            }
            slot.publish_with_state(pane, generation, stale_frame, Some(stale), &tx);
            app.apply_remote_frame(pane, generation, &slot);
            assert_eq!(
                app.remote_workspace_view(app.active_ws).unwrap().state,
                RemoteViewState::Connecting
            );
        }
        slot.publish_with_state(pane, 1, frame("fresh"), Some(state.clone()), &tx);
        app.apply_remote_frame(pane, 1, &slot);
        assert_eq!(
            app.remote_workspace_view(app.active_ws).unwrap().state,
            RemoteViewState::Ready
        );
        app.resize_active_remote_projection();
        assert!(
            receiver.try_recv().is_err(),
            "activation/resize is not presentation"
        );
        app.acknowledge_presented_remote_projection();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ClientMessage::ProjectionPresented {
                epoch: 1,
                event_sequence: 42
            }
        ));
        app.acknowledge_presented_remote_projection();
        assert!(receiver.try_recv().is_err());
        app.active_ws = 0;
        app.resize_active_remote_projection();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ClientMessage::ProjectionInterest {
                epoch: 2,
                active: false,
                ..
            }
        ));
        app.active_ws = 1;
        app.resize_active_remote_projection();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ClientMessage::ProjectionInterest {
                epoch: 3,
                active: true,
                ..
            }
        ));
        slot.publish_with_state(pane, 1, frame("old-selection"), Some(state), &tx);
        app.apply_remote_frame(pane, 1, &slot);
        assert_eq!(
            app.remote_workspace_view(app.active_ws).unwrap().state,
            RemoteViewState::Connecting
        );
    }

    #[test]
    fn remote_projection_capability_requires_server_generation() {
        let mut response = json!({"result":{"workspaces":[], "event_sequence":0,
            "remote_display":protocol::remote_display_capabilities()}});
        assert!(parse_remote_snapshot(&response, RemoteBinaryLocation::Path).is_err());
        response["result"]["server_generation"] = json!("boot");
        assert!(
            parse_remote_snapshot(&response, RemoteBinaryLocation::Path)
                .unwrap()
                .display
                .projection
        );
        response["result"]
            .as_object_mut()
            .unwrap()
            .remove("remote_display");
        assert!(
            !parse_remote_snapshot(&response, RemoteBinaryLocation::Path)
                .unwrap()
                .display
                .projection
        );
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
                "agent": "codex", "terminal_id": "owner-terminal", "is_agent": true,
                "agent_status": "blocked", "focused": true}]}]
        }]}});
        let snapshot = parse_remote_snapshot(&response, RemoteBinaryLocation::Path).unwrap();
        app.apply_remote_session_discovered(
            RemoteSession::new("dev-207", "api").unwrap(),
            Ok(snapshot),
        );
        let agents = app.dispatch("agent.list", &json!({})).unwrap();
        let agent = agents["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["host"] == "dev-207")
            .unwrap();
        assert_eq!(agent["workspace_id"], app.workspaces[app.active_ws].id);
        assert_eq!(agent["owner_workspace_id"], "workspace_remote");
        assert_eq!(agent["terminal_id"], "owner-terminal");
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

    pub(crate) fn add_remote_workspace(
        app: &mut App,
    ) -> (PaneId, mpsc::Receiver<ClientMessage>, PathBuf) {
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
                history: Vec::new(),
                scheduled: Vec::new(),
                target: target.clone(),
                state: RemoteViewState::Ready,
                error: None,
                frame: Some(frame("r")),
                generation: 1,
                input: Some(input.into()),
                projection: Box::default(),
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

    /// Restore only a native dashboard: these UI tests own no PTY, shell,
    /// server, SSH connection, or terminal client.
    pub(crate) fn remote_ui_app() -> App {
        let (tx, _rx) = mpsc::channel();
        let snapshot = crate::persist::SessionSnapshot {
            version: 1,
            active_ws: 0,
            closed_workspace_paths: Vec::new(),
            workspaces: vec![crate::persist::WsSnap {
                id: "workspace_remote_ui_fixture".into(),
                name: "owner".into(),
                cwd: crate::persist::config_dir().join("remote-ui-fixture"),
                active_tab: 0,
                pinned: false,
                tabs: vec![crate::persist::TabSnap {
                    id: "tab_remote_ui_fixture".into(),
                    tree: crate::layout::LayoutTree::Leaf(1),
                    focus: 1,
                    panes: Vec::new(),
                    git: false,
                    orch: true,
                    mission: false,
                    name: None,
                }],
            }],
        };
        let mut app = App::from_snapshot(snapshot, tx).expect("native-only app");
        app.workspaces[0].tabs[0].name = Some("remote-tab".into());
        assert!(app.panes.is_empty());
        app
    }

    #[test]
    fn remote_hidden_sidebar_reopen_is_visible_clickable_and_outside_owner() {
        use ratatui::crossterm::event::MouseButton;
        let _env = crate::persist::test_env("remote-sidebar-reopen");
        for size in [(140, 40), (48, 22)] {
            for side in [super::super::Side::Left, super::super::Side::Right] {
                let mut app = remote_ui_app();
                let (_, receiver, _) = add_remote_workspace(&mut app);
                if side == super::super::Side::Right {
                    app.sidebars.right.docks = vec![super::super::DockKind::Agents];
                }
                app.sidebars.get_mut(side).visible = false;
                remote_ui_buffer(&mut app, size);
                let toggle = match side {
                    super::super::Side::Left => app.sidebar_toggle_rect,
                    super::super::Side::Right => app.right_sidebar_toggle_rect,
                }
                .expect("hidden remote sidebar must retain a reopen button");
                assert!(!app.pane_content_rects[0]
                    .1
                    .contains((toggle.x, toggle.y).into()));
                for kind in [
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Up(MouseButton::Left),
                ] {
                    app.handle_event(AppEvent::Mouse(MouseEvent {
                        kind,
                        column: toggle.x + 1,
                        row: toggle.y,
                        modifiers: KeyModifiers::NONE,
                    }));
                }
                assert!(app.sidebars.get(side).visible);
                assert!(
                    receiver.try_recv().is_err(),
                    "reopen click cannot reach remote tabs"
                );
                remote_ui_buffer(&mut app, size);
                assert!(app.pane_content_rects[0].1.height > 0);
            }
        }
    }

    fn navigation_projection(
        app: &mut App,
        host: &str,
        session: &str,
        workspace_id: &str,
    ) -> (PaneId, mpsc::Receiver<ClientMessage>) {
        let (pane, receiver, _) = add_remote_workspace(app);
        let target = RemoteWorkspaceRef {
            host: host.into(),
            session: session.into(),
            workspace_id: workspace_id.into(),
        };
        let workspace = &mut app.workspaces[app.active_ws];
        workspace.id = format!("projection_{}", pane.0);
        workspace.remote = Some(target.clone());
        let Some(ViewKind::Remote(view)) = app.views.get_mut(&pane) else {
            panic!("native projection fixture missing");
        };
        view.target = target;
        (pane, receiver)
    }

    #[test]
    fn remote_navigation_workspace_resolves_exact_owner_without_touching_other_projections() {
        let _env = crate::persist::test_env("remote-navigation-owner");
        let mut app = remote_ui_app();
        let (source, source_input) = navigation_projection(&mut app, "dev-207", "api", "source");
        let source_index = app.active_ws;
        let (_, other_host) = navigation_projection(&mut app, "dev-208", "api", "destination");
        let (_, other_session) = navigation_projection(&mut app, "dev-207", "other", "destination");
        let (_, destination_input) =
            navigation_projection(&mut app, "dev-207", "api", "destination");
        let destination_index = app.active_ws;
        // Matching a local stable id must not confer remote ownership either.
        app.workspaces[0].id = "destination".into();
        app.active_ws = source_index;
        let count = app.workspaces.len();

        app.apply_remote_effect(RemoteEffect::Workspace {
            pane: source,
            generation: 1,
            workspace_id: "destination".into(),
        });

        assert_eq!(app.active_ws, destination_index);
        assert_eq!(app.workspaces.len(), count);
        assert!(app.pending_remote_navigation.is_none());
        assert!(app.remote_session_watchers.is_empty());
        assert!(app.panes.is_empty());
        assert!(!app.detach_requested && !app.should_quit);
        for receiver in [source_input, other_host, other_session, destination_input] {
            assert!(
                receiver.try_recv().is_err(),
                "navigation wrote to an unrelated owner"
            );
        }
    }

    #[test]
    fn remote_navigation_pending_is_bounded_and_next_snapshot_focuses_the_reopened_target() {
        let _env = crate::persist::test_env("remote-navigation-pending");
        let mut app = remote_ui_app();
        let (source, _source_input) = navigation_projection(&mut app, "dev-207", "api", "source");
        let source_index = app.active_ws;
        let owner = RemoteSession::new("dev-207", "api").unwrap();
        let target = RemoteWorkspaceRef {
            host: owner.host.clone(),
            session: owner.session.clone(),
            workspace_id: "destination".into(),
        };
        let other_owner = RemoteWorkspaceRef {
            host: "dev-208".into(),
            ..target.clone()
        };
        app.closed_remote_workspaces
            .extend([target.clone(), other_owner.clone()]);

        assert!(app
            .apply_remote_workspace_navigation(source, 0, "destination".into())
            .is_none());
        assert!(app.pending_remote_navigation.is_none());
        assert!(app.closed_remote_workspaces.contains(&target));
        assert_eq!(
            app.apply_remote_workspace_navigation(source, 1, "first".into()),
            Some(owner.clone())
        );
        assert_eq!(
            app.apply_remote_workspace_navigation(source, 1, "destination".into()),
            Some(owner.clone())
        );
        assert_eq!(
            app.pending_remote_navigation.as_ref().unwrap().target,
            target
        );
        assert!(!app.closed_remote_workspaces.contains(&target));
        assert!(app.closed_remote_workspaces.contains(&other_owner));

        // Model the projection installed by discovery without starting its SSH
        // worker. Applying the real snapshot method below must consume pending.
        let (_, _destination_input) =
            navigation_projection(&mut app, "dev-207", "api", "destination");
        let destination_index = app.active_ws;
        app.active_ws = source_index;
        let metadata = ["source", "destination"]
            .into_iter()
            .map(|id| RemoteWorkspaceMeta {
                agents: Vec::new(),
                history: Vec::new(),
                scheduled: Vec::new(),
                worktree: None,
                id: id.into(),
                name: id.into(),
                cwd: "/srv/api".into(),
                branch: None,
            })
            .collect();
        app.apply_remote_session_discovered(
            owner,
            Ok(RemoteSessionSnapshot {
                display: RemoteDisplay::default(),
                event_sequence: 20,
                workspaces: metadata,
            }),
        );

        assert_eq!(app.active_ws, destination_index);
        assert!(app.pending_remote_navigation.is_none());
        assert!(app.closed_remote_workspaces.contains(&other_owner));
        assert!(app.remote_session_watchers.is_empty());
        assert!(app.panes.is_empty());
    }

    #[test]
    fn remote_navigation_pending_is_discarded_on_source_leave_generation_change_or_disconnect() {
        let _env = crate::persist::test_env("remote-navigation-source-lifetime");
        let mut app = remote_ui_app();
        let (source, _source_input) = navigation_projection(&mut app, "dev-207", "api", "source");
        let source_index = app.active_ws;
        assert!(app
            .apply_remote_workspace_navigation(source, 1, "destination".into())
            .is_some());
        app.active_ws = 0;
        app.handle_event(AppEvent::RemoteEffect {
            effect: RemoteEffect::Notify("fixture".into()),
        });
        assert!(app.pending_remote_navigation.is_none());

        app.active_ws = source_index;
        assert!(app
            .apply_remote_workspace_navigation(source, 1, "destination".into())
            .is_some());
        let Some(ViewKind::Remote(view)) = app.views.get_mut(&source) else {
            unreachable!()
        };
        view.generation = 2;
        app.discard_stale_remote_navigation();
        assert!(app.pending_remote_navigation.is_none());

        assert!(app
            .apply_remote_workspace_navigation(source, 2, "destination".into())
            .is_some());
        app.apply_remote_projection_closed(source, 1, "old bridge".into());
        assert!(
            app.pending_remote_navigation.is_some(),
            "old bridge canceled the current source"
        );
        app.apply_remote_projection_closed(source, 2, "current bridge".into());
        assert!(app.pending_remote_navigation.is_none());
        assert!(!app.detach_requested && !app.should_quit);
    }

    #[test]
    fn remote_navigation_topology_disconnect_drops_pending_without_detaching_the_display() {
        let _env = crate::persist::test_env("remote-navigation-topology-close");
        let mut app = remote_ui_app();
        let (source, _source_input) = navigation_projection(&mut app, "dev-207", "api", "source");
        assert!(app
            .apply_remote_workspace_navigation(source, 1, "destination".into())
            .is_some());
        app.apply_remote_session_watcher_closed(
            RemoteSession::new("dev-208", "api").unwrap(),
            "other owner".into(),
        );
        assert!(app.pending_remote_navigation.is_some());
        app.apply_remote_session_watcher_closed(
            RemoteSession::new("dev-207", "api").unwrap(),
            "source owner".into(),
        );
        assert!(app.pending_remote_navigation.is_none());
        assert!(!app.detach_requested && !app.should_quit);
    }

    #[test]
    fn remote_navigation_session_and_detach_are_fenced_to_the_active_source() {
        let _env = crate::persist::test_env("remote-navigation-session-detach");
        let mut app = remote_ui_app();
        let (source, source_input) = navigation_projection(&mut app, "dev-207", "api", "source");
        let source_index = app.active_ws;
        assert_eq!(
            app.remote_session_navigation_target(source, 1, "remote-literal-name"),
            Some(RemoteSession::new("dev-207", "remote-literal-name").unwrap())
        );
        assert!(app
            .remote_session_navigation_target(source, 1, "../invalid")
            .is_none());
        assert!(app
            .remote_session_navigation_target(source, 0, "default")
            .is_none());

        app.active_ws = 0;
        assert!(app
            .remote_session_navigation_target(source, 1, "default")
            .is_none());
        for effect in [
            RemoteEffect::Session {
                pane: source,
                generation: 1,
                name: "default".into(),
            },
            RemoteEffect::Detach {
                pane: source,
                generation: 1,
            },
        ] {
            app.apply_remote_effect(effect);
        }
        assert!(app.named_session_menu.is_none());
        assert!(app.pending_session_switch.is_none());
        assert!(!app.detach_requested && !app.should_quit);
        app.active_ws = source_index;
        app.apply_remote_effect(RemoteEffect::Detach {
            pane: source,
            generation: 0,
        });
        assert!(!app.detach_requested);

        app.apply_remote_effect(RemoteEffect::Detach {
            pane: source,
            generation: 1,
        });
        assert!(app.detach_requested);
        assert!(!app.should_quit, "owner Exit stopped the local server");
        assert!(
            source_input.try_recv().is_err(),
            "owner Exit wrote lifecycle input to its server"
        );
        assert!(app.panes.is_empty());
    }

    fn remote_ui_buffer(app: &mut App, size: (u16, u16)) -> ratatui::buffer::Buffer {
        let area = Rect::new(0, 0, size.0, size.1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        let mut target = crate::ui::RenderTarget::new(&mut buffer, area);
        crate::ui::render_into(&mut target, app);
        buffer
    }

    fn remote_ui_owner_frame(owner: &mut App, size: (u16, u16), workspace_only: bool) -> FrameData {
        let area = Rect::new(0, 0, size.0, size.1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        let mut target = crate::ui::RenderTarget::new(&mut buffer, area);
        if workspace_only {
            crate::ui::render_workspace_interactive(&mut target, owner, 0);
        } else {
            crate::ui::render_into(&mut target, owner);
        }
        let cursor = target.cursor();
        let cursor_visible = target.cursor_visible();
        protocol::frame_from_buffer(&buffer, cursor, cursor_visible)
    }

    fn remote_ui_visible_text(
        buffer: &ratatui::buffer::Buffer,
        rect: Rect,
        text: &str,
    ) -> (u16, u16) {
        let width = text.chars().count() as u16;
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right().saturating_sub(width).saturating_add(1) {
                let visible: String = (x..x + width)
                    .map(|column| buffer[(column, y)].symbol())
                    .collect();
                if visible == text {
                    return (x, y);
                }
            }
        }
        panic!("visible owner UI text {text:?} was not rendered in {rect:?}");
    }

    #[test]
    fn remote_visible_tab_plus_and_mobile_menu_reach_owner_coordinates() {
        let _env = crate::persist::test_env("remote-ui-visible-controls");
        for workspace_only in [false, true] {
            for size in [(140, 40), (48, 22)] {
                let mut owner = remote_ui_app();
                let mut outer = remote_ui_app();
                if !workspace_only {
                    outer.workspaces.clear();
                }
                let (pane, receiver, _) = add_remote_workspace(&mut outer);
                remote_ui_buffer(&mut outer, size);
                let area = outer.pane_content_rects[0].1;
                let frame =
                    remote_ui_owner_frame(&mut owner, (area.width, area.height), workspace_only);
                let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) else {
                    unreachable!()
                };
                view.frame = Some(frame);
                let visible = remote_ui_buffer(&mut outer, size);
                let (label_x, label_y) = remote_ui_visible_text(&visible, area, "remote-tab");
                let (point, owner_hit) = if owner.compact {
                    let menu = owner.switcher_button_rect.expect("mobile menu");
                    (
                        remote_ui_visible_text(
                            &visible,
                            Rect::new(area.x, label_y, area.width, 1),
                            &owner.catalog.act_open_menu.to_uppercase(),
                        ),
                        menu,
                    )
                } else {
                    let plus = owner
                        .tab_rects
                        .iter()
                        .find(|(index, _)| *index == owner.ws().tabs.len())
                        .expect("owner new-tab button")
                        .1;
                    let after_label = label_x + "remote-tab".len() as u16;
                    (
                        remote_ui_visible_text(
                            &visible,
                            Rect::new(after_label, label_y, area.right() - after_label, 1),
                            "+",
                        ),
                        plus,
                    )
                };
                outer.handle_event(AppEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left),
                    column: point.0,
                    row: point.1,
                    modifiers: KeyModifiers::NONE,
                }));
                let ClientMessage::Mouse(mouse) = receiver.try_recv().expect("owner input") else {
                    panic!("click did not become owner mouse input")
                };
                assert!(
                    owner_hit.contains((mouse.column, mouse.row).into()),
                    "visible click missed owner control: workspace_only={workspace_only}, \
                     viewport={size:?}, click={point:?}, owner=({}, {}), hit={owner_hit:?}",
                    mouse.column,
                    mouse.row,
                );
                assert_eq!(outer.ws().tabs.len(), 1, "no local shadow tab was created");
                assert!(outer.panes.is_empty());
                assert!(receiver.try_recv().is_err());
            }
        }
    }

    #[test]
    fn remote_pointer_release_outside_projection_finishes_owner_gesture() {
        use ratatui::crossterm::event::MouseButton;
        let _env = crate::persist::test_env("remote-pointer-capture");
        let mut app = remote_ui_app();
        let (pane, receiver, _) = add_remote_workspace(&mut app);
        app.pane_content_rects = vec![(pane, Rect::new(30, 2, 70, 25))];
        for (kind, point, expected) in [
            (MouseEventKind::Down(MouseButton::Left), (35, 5), (5, 3)),
            (MouseEventKind::Drag(MouseButton::Left), (0, 0), (0, 0)),
            (MouseEventKind::Up(MouseButton::Left), (110, 35), (69, 24)),
        ] {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
            let ClientMessage::Mouse(mouse) = receiver.try_recv().expect("captured owner gesture")
            else {
                panic!("gesture must reach owner")
            };
            assert_eq!((mouse.column, mouse.row), expected);
            assert_eq!(mouse.kind, kind);
        }
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            receiver.try_recv().is_err(),
            "the released gesture no longer captures local input"
        );
    }

    #[test]
    fn remote_menu_outside_content_click_dismisses_without_activating_owner_content() {
        use crate::orch::TaskWorkerMode;
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-menu-dismiss-content");
        for workspace_only in [false, true] {
            let mut owner = remote_ui_app();
            let mut second_tab = Tab::panes(TileLayout::new(PaneId::alloc()));
            second_tab.orch = true;
            owner.workspaces[0].tabs.push(second_tab);
            let mut outer = remote_ui_app();
            if !workspace_only {
                outer.workspaces.clear();
            }
            let (pane, receiver, _) = add_remote_workspace(&mut outer);
            remote_ui_buffer(&mut outer, (180, 48));
            let area = outer.pane_content_rects[0].1;
            let owner_frame =
                remote_ui_owner_frame(&mut owner, (area.width, area.height), workspace_only);
            let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) else {
                unreachable!()
            };
            view.frame = Some(owner_frame);
            let visible = remote_ui_buffer(&mut outer, (180, 48));
            let tab = remote_ui_visible_text(&visible, area, "remote-tab");
            for kind in [
                MouseEventKind::Down(MouseButton::Right),
                MouseEventKind::Up(MouseButton::Right),
            ] {
                outer.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: tab.0,
                    row: tab.1,
                    modifiers: KeyModifiers::NONE,
                }));
                let ClientMessage::Mouse(mouse) =
                    receiver.try_recv().expect("owner tab right click")
                else {
                    panic!("tab right click did not reach owner");
                };
                owner.handle_event(AppEvent::Mouse(mouse));
            }
            assert!(owner.tab_menu.is_some(), "owner tab menu did not open");
            let owner_frame =
                remote_ui_owner_frame(&mut owner, (area.width, area.height), workspace_only);
            let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) else {
                unreachable!()
            };
            view.frame = Some(owner_frame);
            let visible = remote_ui_buffer(&mut outer, (180, 48));
            remote_ui_visible_text(&visible, area, "Move");

            let control = owner
                .orch_hits
                .iter()
                .find_map(|(hit, rect)| {
                    matches!(
                        hit,
                        super::super::OrchHit::FlowMode(TaskWorkerMode::Workspace)
                    )
                    .then_some(*rect)
                })
                .expect("native content control");
            let point = (control.x + control.width / 2, control.y);
            assert!(
                !owner
                    .tab_menu
                    .as_ref()
                    .unwrap()
                    .items
                    .iter()
                    .any(|(_, rect)| rect.contains(point.into())),
                "fixture content control is covered by menu"
            );
            assert_eq!(owner.orch_flow_mode, TaskWorkerMode::Worktree);
            // The first full gesture closes the popup. The second must reach
            // the native owner control, proving this point is actionable while
            // remaining entirely in memory (no terminal or SSH child required).
            for (click, expected) in [
                (1, TaskWorkerMode::Worktree),
                (2, TaskWorkerMode::Workspace),
            ] {
                for kind in [
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Up(MouseButton::Left),
                ] {
                    outer.handle_event(AppEvent::Mouse(MouseEvent {
                        kind,
                        column: area.x + point.0,
                        row: area.y + point.1,
                        modifiers: KeyModifiers::NONE,
                    }));
                    let ClientMessage::Mouse(mouse) =
                        receiver.try_recv().expect("owner content click")
                    else {
                        panic!("content click did not reach owner");
                    };
                    owner.handle_event(AppEvent::Mouse(mouse));
                }
                assert!(
                    owner.tab_menu.is_none(),
                    "owner menu survived outside click {click}"
                );
                assert_eq!(
                    owner.orch_flow_mode, expected,
                    "outside click {click} leaked through popup"
                );
            }
            assert!(receiver.try_recv().is_err());
            assert!(outer.panes.is_empty() && owner.panes.is_empty());
        }
    }

    #[test]
    fn remote_menu_bar_overflow_outside_click_is_consumed_before_owner_input() {
        use crate::bar::{BarRegion, BarSegment, BarTone, BarWidget, BarWidgetKey};
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-menu-bar-outside");
        let mut app = remote_ui_app();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        for index in 0..4 {
            app.bar
                .push_widget(
                    BarWidget::new(
                        BarWidgetKey::new("fixture", format!("wide-{index}")),
                        BarRegion::BottomRight,
                        vec![BarSegment::text(
                            "visible overflow fixture ".repeat(3),
                            BarTone::Normal,
                        )],
                        Vec::new(),
                        50,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let visible = remote_ui_buffer(&mut app, (180, 48));
        let overflow_hit = app
            .bar
            .overflow_hits
            .iter()
            .find(|hit| hit.region == BarRegion::BottomRight)
            .expect("real rendered bar overflow control")
            .rect;
        let point = remote_ui_visible_text(&visible, overflow_hit, "…");
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            app.bar.overflow.is_some(),
            "visible overflow control did not open popup"
        );
        assert!(receiver.try_recv().is_err(), "bar control leaked to owner");
        let visible = remote_ui_buffer(&mut app, (180, 48));
        let popup = app.bar.overflow.as_ref().unwrap().rect;
        remote_ui_visible_text(&visible, popup, "Luvus Bar");
        let area = app.pane_content_rects[0].1;
        let point = (area.x + 2, area.y + 2);
        assert!(!popup.contains(point.into()));
        for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(
                app.bar.overflow.is_some(),
                "wheel unexpectedly dismissed read-only popup"
            );
            assert!(receiver.try_recv().is_err(), "popup wheel leaked to owner");
        }
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(app.bar.overflow.is_none());
        assert!(receiver.try_recv().is_err(), "popup Escape leaked to owner");

        for (button, size) in [
            (MouseButton::Left, (140, 38)),
            (MouseButton::Right, (180, 48)),
        ] {
            remote_ui_buffer(&mut app, (180, 48));
            let hit = app
                .bar
                .overflow_hits
                .iter()
                .find(|hit| hit.region == BarRegion::BottomRight)
                .unwrap()
                .rect;
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                app.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: hit.x,
                    row: hit.y,
                    modifiers: KeyModifiers::NONE,
                }));
            }
            assert!(app.bar.overflow.is_some());
            assert!(receiver.try_recv().is_err());
            let visible = remote_ui_buffer(&mut app, size);
            let popup = app.bar.overflow.as_ref().unwrap().rect;
            remote_ui_visible_text(&visible, popup, "Luvus Bar");
            let area = app.pane_content_rects[0].1;
            let point = (area.x + 2, area.y + 2);
            assert!(!popup.contains(point.into()));
            for kind in [MouseEventKind::Down(button), MouseEventKind::Up(button)] {
                app.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: point.0,
                    row: point.1,
                    modifiers: KeyModifiers::NONE,
                }));
            }
            assert!(
                app.bar.overflow.is_none(),
                "remote content click left outer popup open"
            );
            assert!(
                receiver.try_recv().is_err(),
                "popup dismissal gesture also interacted with owner content"
            );
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                app.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: point.0,
                    row: point.1,
                    modifiers: KeyModifiers::NONE,
                }));
                let ClientMessage::Mouse(sent) =
                    receiver.try_recv().expect("next real owner gesture")
                else {
                    panic!("next owner gesture missing");
                };
                assert_eq!(sent.kind, kind);
            }
            assert!(receiver.try_recv().is_err());
        }
        assert!(app.panes.is_empty());
    }

    #[test]
    fn remote_menu_outer_workspace_dismissal_consumes_release_before_owner_input() {
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-menu-outer-workspace");
        for button in [MouseButton::Left, MouseButton::Right] {
            let mut app = remote_ui_app();
            let (_pane, receiver, _) = add_remote_workspace(&mut app);
            remote_ui_buffer(&mut app, (180, 48));
            let row = app
                .ws_rects
                .iter()
                .find(|(index, _)| *index == app.active_ws)
                .expect("visible remote workspace sidebar row")
                .1;
            for kind in [
                MouseEventKind::Down(MouseButton::Right),
                MouseEventKind::Up(MouseButton::Right),
            ] {
                app.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: row.x + 2,
                    row: row.y,
                    modifiers: KeyModifiers::NONE,
                }));
            }
            assert!(
                app.ws_menu.is_some(),
                "real sidebar right click did not open workspace menu"
            );
            let visible = remote_ui_buffer(&mut app, (140, 38));
            remote_ui_visible_text(&visible, visible.area, app.catalog.menu_rename);
            let area = app.pane_content_rects[0].1;
            let point = (area.right() - 3, area.y + area.height / 2);
            assert!(!app
                .ws_menu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|(_, rect)| rect.contains(point.into())));
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(app.ws_menu.is_some());
            assert!(
                receiver.try_recv().is_err(),
                "outer workspace menu wheel leaked to owner"
            );
            for kind in [MouseEventKind::Down(button), MouseEventKind::Up(button)] {
                app.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: point.0,
                    row: point.1,
                    modifiers: KeyModifiers::NONE,
                }));
            }
            assert!(app.ws_menu.is_none());
            assert!(
                receiver.try_recv().is_err(),
                "outer workspace dismissal leaked its release to owner"
            );
            // With the popup gone, an ordinary sidebar click can select the
            // local workspace without retaining or executing its old popup.
            remote_ui_buffer(&mut app, (140, 38));
            let local = app
                .ws_rects
                .iter()
                .find(|(index, _)| *index == 0)
                .unwrap()
                .1;
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: local.x + 2,
                row: local.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert_eq!(app.active_ws, 0);
            assert!(app.ws_menu.is_none());
            assert!(receiver.try_recv().is_err());
            assert!(app.panes.is_empty());
        }
    }

    #[test]
    fn remote_menu_local_sidebar_drag_crossing_owner_content_stays_local() {
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-menu-local-sidebar-drag");
        let mut app = remote_ui_app();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        remote_ui_buffer(&mut app, (180, 48));
        let seam = app.left_seam.expect("visible local sidebar seam");
        let width = app.sidebars.left.width;
        let row = seam.y + 3;
        let column = seam.x + 8;
        assert!(app.pane_content_rects[0].1.contains((column, row).into()));
        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), seam.x),
            (MouseEventKind::Drag(MouseButton::Left), column),
            (MouseEventKind::Up(MouseButton::Left), column),
        ] {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }));
        }
        assert_eq!(app.sidebars.left.width, width + 8);
        assert!(
            app.sidebar_resize.is_none(),
            "local resize never received release inside remote frame"
        );
        assert!(
            receiver.try_recv().is_err(),
            "local divider gesture leaked to owner"
        );
        assert!(app.panes.is_empty());
    }

    #[test]
    fn remote_folder_picker_host_choice_and_owner_browse_clicks_stay_on_the_selected_host() {
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-folder-picker-host-browse");
        let root = crate::persist::config_dir().join("owner-folder-fixture");
        std::fs::create_dir_all(root.join("remote-child")).unwrap();
        let mut owner = remote_ui_app();
        owner.workspaces[0].cwd = root.clone();
        let mut outer = remote_ui_app();
        outer.remote_merge_enabled = true;
        outer.config.remote_hosts = vec!["dev-207".into()];
        let session = crate::session::display_name();
        let (pane, receiver) =
            navigation_projection(&mut outer, "dev-207", &session, "owner-workspace");
        let visible = remote_ui_buffer(&mut outer, (180, 48));
        let plus = outer.new_ws_rect.expect("outer Open workspace button");
        let point = remote_ui_visible_text(&visible, plus, "+");
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            outer.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
        }
        let visible = remote_ui_buffer(&mut outer, (140, 38));
        assert!(outer.picker.as_ref().unwrap().hosts.is_some());
        let host_row = outer
            .picker_rects
            .iter()
            .find_map(|(hit, rect)| (*hit == super::super::PickerHit::Row(1)).then_some(*rect))
            .expect("selected SSH host row");
        let point = remote_ui_visible_text(&visible, host_row, "dev-207");
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            outer.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
        }
        let ClientMessage::Command(command) = receiver.try_recv().expect("owner picker command")
        else {
            panic!("host choice did not target the owner's folder picker");
        };
        assert_eq!(command, "open_local_workspace");
        assert!(outer.picker.is_none());
        assert!(
            receiver.try_recv().is_err(),
            "host-choice release leaked into new owner"
        );
        owner.handle_event(AppEvent::ClientCommand(command));
        assert_eq!(owner.picker.as_ref().unwrap().path, root);
        assert!(owner.picker.as_ref().unwrap().worktrees.is_none());

        for (size, target) in [
            ((140, 38), "remote-child"),
            ((180, 48), ".."),
            ((140, 38), "esc"),
        ] {
            remote_ui_buffer(&mut outer, size);
            let area = outer.pane_content_rects[0].1;
            let frame = remote_ui_owner_frame(&mut owner, (area.width, area.height), true);
            let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) else {
                unreachable!()
            };
            view.frame = Some(frame);
            let visible = remote_ui_buffer(&mut outer, size);
            let point = if target == "esc" {
                let cancel = owner
                    .picker_rects
                    .iter()
                    .find_map(|(hit, rect)| {
                        (*hit == super::super::PickerHit::Hint(KeyCode::Esc)).then_some(*rect)
                    })
                    .expect("visible owner cancel footer");
                remote_ui_visible_text(
                    &visible,
                    Rect::new(
                        area.x + cancel.x,
                        area.y + cancel.y,
                        cancel.width,
                        cancel.height,
                    ),
                    "esc",
                )
            } else {
                remote_ui_visible_text(&visible, area, target)
            };
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                outer.handle_event(AppEvent::Mouse(MouseEvent {
                    kind,
                    column: point.0,
                    row: point.1,
                    modifiers: KeyModifiers::NONE,
                }));
                let ClientMessage::Mouse(mouse) = receiver.try_recv().expect("owner picker mouse")
                else {
                    panic!("owner picker click not forwarded");
                };
                owner.handle_event(AppEvent::Mouse(mouse));
            }
            if target == "remote-child" {
                assert_eq!(
                    owner.picker.as_ref().unwrap().path,
                    root.join("remote-child")
                );
            } else if target == ".." {
                assert_eq!(owner.picker.as_ref().unwrap().path, root);
            } else {
                assert!(owner.picker.is_none());
            }
            assert!(outer.picker.is_none());
            assert_eq!(outer.workspaces.len(), 2);
            assert_eq!(owner.workspaces.len(), 1);
        }
        assert!(outer.remote_session_watchers.is_empty());
        assert!(outer.panes.is_empty() && owner.panes.is_empty());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn remote_folder_picker_same_owner_host_choice_keeps_the_active_workspace() {
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-folder-picker-active-owner");
        let mut app = remote_ui_app();
        app.remote_merge_enabled = true;
        app.config.remote_hosts = vec!["dev-207".into()];
        let session = crate::session::display_name();
        let (_, first_input) = navigation_projection(&mut app, "dev-207", &session, "workspace-a");
        let (_, active_input) = navigation_projection(&mut app, "dev-207", &session, "workspace-b");
        let active = app.active_ws;
        app.handle_event(AppEvent::ClientCommand("new_node".into()));
        let visible = remote_ui_buffer(&mut app, (140, 38));
        let row = app
            .picker_rects
            .iter()
            .find_map(|(hit, rect)| (*hit == super::super::PickerHit::Row(1)).then_some(*rect))
            .expect("remote host choice");
        let point = remote_ui_visible_text(&visible, row, "dev-207");
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
        }
        assert_eq!(
            app.active_ws, active,
            "choosing the current owner switched to its first workspace"
        );
        assert!(
            matches!(active_input.try_recv(), Ok(ClientMessage::Command(command)) if command == "open_local_workspace")
        );
        assert!(
            first_input.try_recv().is_err(),
            "picker opened on a different workspace's directory"
        );
        assert!(active_input.try_recv().is_err());
        assert!(app.picker.is_none());
        assert!(app.remote_session_watchers.is_empty());
        assert!(app.panes.is_empty());
    }

    #[test]
    fn remote_folder_picker_compact_cancel_remains_visible_and_clickable() {
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-folder-picker-compact-cancel");
        let root = crate::persist::config_dir().join("folder-fixture");
        std::fs::create_dir_all(&root).unwrap();
        for workspace_only in [false, true] {
            for size in [(140, 38), (48, 22)] {
                let mut app = remote_ui_app();
                app.catalog = &crate::i18n::EN;
                app.workspaces[0].cwd = root.clone();
                app.handle_event(AppEvent::ClientCommand("open_local_workspace".into()));
                let frame = remote_ui_owner_frame(&mut app, size, workspace_only);
                let cancel = app.picker_rects.iter().find_map(|(hit, rect)| {
                    (*hit == super::super::PickerHit::Hint(KeyCode::Esc)).then_some(*rect)
                }).unwrap_or_else(|| panic!("folder picker has no clickable cancel: owner_projection={workspace_only}, size={size:?}"));
                assert!(cancel.width >= 3 && cancel.right() <= size.0 && cancel.bottom() <= size.1);
                let label: String = (cancel.x..cancel.x + 3)
                    .map(|column| {
                        frame.cells
                            [usize::from(cancel.y) * usize::from(frame.width) + usize::from(column)]
                        .symbol
                        .as_str()
                    })
                    .collect();
                assert_eq!(label, "esc");
                for kind in [
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Up(MouseButton::Left),
                ] {
                    app.handle_event(AppEvent::Mouse(MouseEvent {
                        kind,
                        column: cancel.x,
                        row: cancel.y,
                        modifiers: KeyModifiers::NONE,
                    }));
                }
                assert!(app.picker.is_none());
                assert!(app.panes.is_empty());
            }
        }
    }

    #[test]
    fn remote_right_click_menu_anchor_and_visible_actions_survive_resize() {
        use ratatui::crossterm::event::MouseButton;

        let _env = crate::persist::test_env("remote-menu-anchor-resize");
        for workspace_only in [false, true] {
            let mut owner = remote_ui_app();
            let mut second_tab = Tab::panes(TileLayout::new(PaneId::alloc()));
            second_tab.orch = true;
            owner.workspaces[0].tabs.push(second_tab);
            let mut outer = remote_ui_app();
            if !workspace_only {
                outer.workspaces.clear();
            }
            let (pane, receiver, _) = add_remote_workspace(&mut outer);
            remote_ui_buffer(&mut outer, (140, 40));
            let area = outer.pane_content_rects[0].1;
            let frame =
                remote_ui_owner_frame(&mut owner, (area.width, area.height), workspace_only);
            if let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) {
                view.frame = Some(frame);
            }
            let visible = remote_ui_buffer(&mut outer, (140, 40));
            let point = remote_ui_visible_text(&visible, area, "remote-tab");
            outer.handle_event(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            }));
            let ClientMessage::Mouse(mouse) = receiver.try_recv().expect("owner right click")
            else {
                panic!("right click must be forwarded")
            };
            let anchor = (mouse.column, mouse.row);
            assert_eq!(anchor, (point.0 - area.x, point.1 - area.y));
            owner.handle_event(AppEvent::Mouse(mouse));
            assert_eq!(
                owner.tab_menu.as_ref().expect("owner tab menu").anchor,
                anchor
            );
            assert!(
                outer.tab_menu.is_none(),
                "no local context menu over the remote frame"
            );

            // Keep the same popup open while both its containing projection
            // and owner viewport change. Visible rows must still map to the
            // owner's hit rectangles, including the compact bottom sheet.
            for size in [(140, 40), (48, 22), (170, 52)] {
                remote_ui_buffer(&mut outer, size);
                let area = outer.pane_content_rects[0].1;
                let frame =
                    remote_ui_owner_frame(&mut owner, (area.width, area.height), workspace_only);
                if let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) {
                    view.frame = Some(frame);
                }
                let visible = remote_ui_buffer(&mut outer, size);
                for (_, hit) in &owner.tab_menu.as_ref().unwrap().items {
                    if hit.width > 0 && hit.height > 0 {
                        assert!(hit.right() <= area.width && hit.bottom() <= area.height);
                    }
                }
                let point = remote_ui_visible_text(&visible, area, "Move");
                outer.handle_event(AppEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: point.0,
                    row: point.1,
                    modifiers: KeyModifiers::NONE,
                }));
                let ClientMessage::Mouse(mouse) = receiver.try_recv().expect("owner menu hover")
                else {
                    panic!("menu hover must reach its owner")
                };
                let hit = owner
                    .tab_menu
                    .as_ref()
                    .unwrap()
                    .items
                    .iter()
                    .find(|(item, _)| matches!(item, crate::app::TabMenuItem::MoveRight))
                    .expect("move right action")
                    .1;
                assert!(
                    hit.contains((mouse.column, mouse.row).into()),
                    "visible menu action missed its owner after {size:?}"
                );
            }
            assert!(outer.panes.is_empty() && owner.panes.is_empty());
        }
    }

    #[test]
    fn remote_frame_resize_preserves_corners_and_cursor_without_extra_chrome() {
        let _env = crate::persist::test_env("remote-ui-resize-corners");
        for workspace_only in [false, true] {
            let mut owner = remote_ui_app();
            let mut outer = remote_ui_app();
            if !workspace_only {
                outer.workspaces.clear();
            }
            let (pane, receiver, _) = add_remote_workspace(&mut outer);
            for size in [(140, 40), (48, 22), (170, 52)] {
                remote_ui_buffer(&mut outer, size);
                let area = outer.pane_content_rects[0].1;
                outer.resize_active_remote_projection();
                let ClientMessage::Resize { cols, rows } =
                    receiver.try_recv().expect("owner resize")
                else {
                    panic!("expected owner resize before accepting its next frame")
                };
                assert_eq!((cols, rows), (area.width, area.height));
                let mut frame = remote_ui_owner_frame(&mut owner, (cols, rows), workspace_only);
                frame.cells[0].symbol = "L".into();
                frame.cells.last_mut().unwrap().symbol = "Z".into();
                frame.cursor = Some((cols - 1, rows - 1));
                frame.cursor_visible = true;
                let Some(ViewKind::Remote(view)) = outer.views.get_mut(&pane) else {
                    unreachable!()
                };
                view.frame = Some(frame);
                let visible = remote_ui_buffer(&mut outer, size);
                assert_eq!(visible[(area.x, area.y)].symbol(), "L");
                assert_eq!(
                    visible[(area.right() - 1, area.bottom() - 1)].symbol(),
                    "Z",
                    "owner's last row/column must not be cropped after {size:?}"
                );
                assert_eq!(
                    outer.last_cursor,
                    Some((area.right() - 1, area.bottom() - 1))
                );
                outer.resize_active_remote_projection();
                assert!(
                    receiver.try_recv().is_err(),
                    "stable geometry must not resize again"
                );
            }
        }
    }

    #[test]
    fn remote_prefix_help_reaches_the_owner_without_opening_local_help() {
        let _env = crate::persist::test_env("remote-prefix-help");
        let mut app = remote_ui_app();
        let mut owner = remote_ui_app();
        app.prefix = super::super::keys::PrefixSpec::parse("ctrl+b").unwrap();
        owner.prefix = super::super::keys::PrefixSpec::parse("ctrl+a").unwrap();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        app.handle_event(AppEvent::Key(app.prefix.key_event()));
        assert!(app.mode == Mode::Prefix);
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('?'),
            KeyModifiers::NONE,
        )));
        let ClientMessage::PrefixKey(key) = receiver
            .try_recv()
            .expect("the prefix help suffix must reach its owner")
        else {
            panic!("owner must receive a prefix suffix, not the display prefix or command map");
        };
        owner.handle_event(AppEvent::PrefixKey(key));
        assert!(owner.help_open);
        assert!(owner.mode == Mode::Normal);
        assert!(
            !app.help_open,
            "remote prefix help must not open local help"
        );
        assert!(app.mode == Mode::Normal);
        assert!(app.panes.is_empty());
    }

    #[test]
    fn remote_prefix_uses_owner_digits_custom_bindings_and_fixed_scrollback_keys() {
        let _env = crate::persist::test_env("remote-prefix-owner-map");
        let mut app = remote_ui_app();
        let mut owner = remote_ui_app();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        let mut second = Tab::panes(TileLayout::new(PaneId::alloc()));
        second.orch = true;
        owner.workspaces[0].tabs.push(second);
        app.keymap.insert("u".into(), Cmd::PrevTab);
        owner.keymap.insert("u".into(), Cmd::NextTab);
        for suffix in ['2', 'u'] {
            owner.workspaces[0].active_tab = 0;
            app.handle_event(AppEvent::Key(app.prefix.key_event()));
            app.handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Char(suffix),
                KeyModifiers::NONE,
            )));
            let ClientMessage::PrefixKey(key) = receiver.try_recv().unwrap() else {
                panic!("owner must resolve its fixed digits and custom bindings");
            };
            owner.handle_event(AppEvent::PrefixKey(key));
            assert_eq!(owner.workspaces[0].active_tab, 1, "suffix {suffix}");
            assert_eq!(app.workspaces[app.active_ws].active_tab, 0);
        }
        for code in [
            KeyCode::Char('['),
            KeyCode::Char(']'),
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
        ] {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            app.handle_event(AppEvent::Key(app.prefix.key_event()));
            app.handle_event(AppEvent::Key(key));
            assert!(
                matches!(receiver.try_recv().unwrap(), ClientMessage::PrefixKey(received) if received == key)
            );
        }
        assert!(app.panes.is_empty());
        assert!(owner.panes.is_empty());
    }

    #[test]
    fn remote_double_prefix_requests_one_owner_prefix_and_preserves_modal_precedence() {
        let _env = crate::persist::test_env("remote-double-prefix");
        let mut app = remote_ui_app();
        let mut owner = remote_ui_app();
        app.prefix = super::super::keys::PrefixSpec::parse("ctrl+b").unwrap();
        owner.prefix = super::super::keys::PrefixSpec::parse("ctrl+a").unwrap();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        app.handle_event(AppEvent::Key(app.prefix.key_event()));
        app.handle_event(AppEvent::Key(app.prefix.key_event()));
        let ClientMessage::Command(command) = receiver.try_recv().unwrap() else {
            panic!("double display prefix must request the owner's own prefix");
        };
        assert_eq!(command, "send_prefix");
        assert!(
            receiver.try_recv().is_err(),
            "send only one owner prefix request"
        );
        owner.handle_event(AppEvent::ClientCommand(command));
        assert!(owner.mode == Mode::Normal);
        assert!(owner
            .prefix
            .matches(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        owner.help_open = true;
        owner.handle_event(AppEvent::PrefixKey(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(!owner.help_open);
        assert!(
            owner.mode == Mode::Normal,
            "closing an overlay must not leave prefix mode armed"
        );
        for command in [Cmd::OpenSettings, Cmd::Switcher] {
            owner.run_cmd(command);
            match command {
                Cmd::OpenSettings => assert!(owner.settings.is_some()),
                Cmd::Switcher => assert!(owner.switcher),
                _ => unreachable!(),
            }
            owner.handle_event(AppEvent::ClientCommand("send_prefix".into()));
            assert!(
                owner.mode == Mode::Normal,
                "double prefix must retain modal precedence"
            );
            owner.handle_event(AppEvent::PrefixKey(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
            assert!(owner.settings.is_none() && !owner.switcher);
            assert!(
                owner.mode == Mode::Normal,
                "closing a modal must not arm the next ordinary key"
            );
            owner.handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Char('='),
                KeyModifiers::NONE,
            )));
            assert!(
                owner.settings.is_none(),
                "an ordinary key after modal close must not become a prefix command"
            );
        }
        assert!(app.panes.is_empty());
        assert!(owner.panes.is_empty());
    }

    #[test]
    fn remote_prefix_suffixes_route_to_owner_and_local_session_menu_stays_local() {
        let _env = crate::persist::test_env("remote-semantic-shortcuts");
        let mut app = remote_ui_app();
        app.server_mode = true;
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        // The display's prefix is an alias; the owner resolves the suffix.
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
            matches!(receiver.recv().unwrap(), ClientMessage::PrefixKey(received) if received == key)
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
        assert!(slot.take().is_some_and(|next| next.0 == frame("b")));

        slot.publish(pane, 1, frame("c"), &tx);
        assert!(rx.recv().is_ok());
        assert!(slot.take().is_some_and(|next| next.0 == frame("c")));
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
                display: RemoteDisplay::default(),
                event_sequence: 2,
                workspaces: vec![RemoteWorkspaceMeta {
                    agents: Vec::new(),
                    history: Vec::new(),
                    scheduled: Vec::new(),
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
                display: RemoteDisplay::default(),
                event_sequence: 4,
                workspaces: vec![RemoteWorkspaceMeta {
                    agents: Vec::new(),
                    history: Vec::new(),
                    scheduled: Vec::new(),
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
                display: RemoteDisplay::default(),
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
                target: target.clone(),
                generation: 5,
                scope: Arc::new(Default::default()),
                refresh_projections: true,
                retry_delay: None,
                retry_pending: false,
            },
        );
        let snapshot = RemoteSessionSnapshot {
            display: RemoteDisplay::default(),
            event_sequence: 9,
            workspaces: vec![RemoteWorkspaceMeta {
                agents: Vec::new(),
                history: Vec::new(),
                scheduled: Vec::new(),
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

        // Without an enabled host, ordinary owner events cannot retry a
        // disconnected display. Explicit refresh remains available.
        app.apply_remote_session_discovered(target, Ok(snapshot));
        let view = app.remote_workspace_view(app.active_ws).unwrap();
        assert_eq!(view.state, RemoteViewState::Disconnected);
        assert_eq!(view.generation, 2);
    }

    #[test]
    fn remote_retry_coalesces_failures_backs_off_and_cancels_with_the_owner() {
        let _env = crate::persist::test_env("remote-retry-lifecycle");
        let mut app = remote_ui_app();
        let (pane, input, _) = add_remote_workspace(&mut app);
        let target = RemoteSession::new("dev-207", "api").unwrap();
        app.config.remote_hosts.push(target.host.clone());
        app.remote_watcher_generation = 5;
        let old_scope = Arc::new(crate::session::remote::ConnectionScope::default());
        app.remote_session_watchers.insert(
            target.canonical_name(),
            RemoteWatcher {
                target: target.clone(),
                generation: 5,
                scope: old_scope.clone(),
                refresh_projections: false,
                retry_delay: Some(REMOTE_RETRY_INITIAL),
                retry_pending: false,
            },
        );
        app.apply_remote_session_watcher_closed(target.clone(), "bridge closed".into());
        assert!(!old_scope.wait_for_retry(Duration::ZERO));
        assert!(matches!(input.try_recv().unwrap(), ClientMessage::Detach));
        assert!(app.remote_watcher_is_current(&target, 6));
        assert!(app.remote_session_watchers[&target.canonical_name()].retry_pending);
        assert!(!app.send_active_remote(ClientMessage::Command("new_tab".into())));
        let view = app.remote_workspace_view(app.active_ws).unwrap();
        assert_eq!(view.state, RemoteViewState::Connecting);
        assert!(
            view.frame.is_some(),
            "keep the last frame beneath the reconnect notice"
        );
        let projection_generation = view.generation;
        // Both streams can fail for one restart. Neither the late watcher nor
        // old frame/ready/close messages may replace this pending attempt.
        app.handle_event(AppEvent::RemoteSessionWatcherClosed {
            target: target.clone(),
            generation: 5,
            error: "old watcher closed".into(),
        });
        app.apply_remote_projection_closed(pane, 1, "old display closed".into());
        let (sender, receiver) = mpsc::channel();
        app.apply_remote_projection_ready(pane, 1, sender.into());
        assert!(receiver.recv().is_err());
        assert!(app.remote_watcher_is_current(&target, 6));
        assert_eq!(
            app.remote_workspace_view(app.active_ws).unwrap().generation,
            projection_generation
        );
        assert!(app.retry_remote_session(&target, "duplicate failure"));
        assert!(app.remote_watcher_is_current(&target, 6));

        for expected in [1000, 2000, 4000, 8000, 10000, 10000] {
            let previous = app.remote_session_watchers[&target.canonical_name()]
                .scope
                .clone();
            app.apply_remote_session_discovered(target.clone(), Err("owner still stopped".into()));
            assert!(!previous.wait_for_retry(Duration::ZERO));
            let watcher = &app.remote_session_watchers[&target.canonical_name()];
            assert_eq!(watcher.retry_delay, Some(Duration::from_millis(expected)));
            assert!(watcher.retry_pending);
        }
        let pending = app.remote_session_watchers[&target.canonical_name()]
            .scope
            .clone();
        app.config.remote_hosts.clear();
        app.retain_remote_sessions(|_, _| false);
        assert!(!pending.wait_for_retry(Duration::ZERO));
        assert!(!app.retry_remote_session(&target, "late failure"));
        assert!(app.remote_session_watchers.is_empty());
        assert!(app
            .workspaces
            .iter()
            .all(|workspace| workspace.remote.is_none()));
    }

    #[test]
    fn remote_initial_open_failure_does_not_create_an_automatic_retry() {
        let _env = crate::persist::test_env("remote-initial-failure");
        let mut app = remote_ui_app();
        let (pane, _, _) = add_remote_workspace(&mut app);
        let target = RemoteSession::new("dev-207", "api").unwrap();
        app.config.remote_hosts.push(target.host.clone());
        app.remote_session_watchers.insert(
            target.canonical_name(),
            RemoteWatcher {
                target: target.clone(),
                generation: 1,
                scope: Arc::new(Default::default()),
                refresh_projections: true,
                retry_delay: None,
                retry_pending: false,
            },
        );
        app.apply_remote_session_discovered(
            target,
            Err("modified remote-session build required".into()),
        );
        assert!(app.remote_session_watchers.is_empty());
        assert!(matches!(&app.views[&pane], ViewKind::Remote(view)
            if view.state == RemoteViewState::Disconnected));
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
                target: target.clone(),
                generation: 4,
                scope: Arc::new(Default::default()),
                refresh_projections: false,
                retry_delay: None,
                retry_pending: false,
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
        app.apply_remote_projection_ready(pane, 1, input.into());
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
                history: Vec::new(),
                scheduled: Vec::new(),
                target: RemoteWorkspaceRef {
                    host: "dev-207".into(),
                    session: "api".into(),
                    workspace_id: "workspace_remote_2".into(),
                },
                state: RemoteViewState::Ready,
                error: None,
                frame: Some(frame("s")),
                generation: 2,
                input: Some(input.into()),
                projection: Box::default(),
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
                target: target.clone(),
                generation: 5,
                scope: Arc::new(Default::default()),
                refresh_projections: true,
                retry_delay: None,
                retry_pending: false,
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
                display: RemoteDisplay::default(),
                event_sequence: 7,
                workspaces: vec![RemoteWorkspaceMeta {
                    agents: Vec::new(),
                    history: Vec::new(),
                    scheduled: Vec::new(),
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
    fn disabling_one_host_revokes_its_live_and_pending_sessions_before_config_save() {
        let _env = crate::persist::test_env("remote-disable-pending-save");
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let disabled = RemoteSession::new("dev-207", "api-one").unwrap();
        let pending = RemoteSession::new("dev-207", "api-two").unwrap();
        let enabled = RemoteSession::new("dev-other", "api-one").unwrap();
        let (_, disabled_input) =
            navigation_projection(&mut app, &disabled.host, &disabled.session, "disabled");
        let (_, enabled_input) =
            navigation_projection(&mut app, &enabled.host, &enabled.session, "enabled");
        let active_id = app.workspaces[app.active_ws].id.clone();
        for target in [&disabled, &pending, &enabled] {
            app.remote_session_watchers.insert(
                target.canonical_name(),
                RemoteWatcher {
                    target: target.clone(),
                    generation: 5,
                    scope: Arc::new(Default::default()),
                    refresh_projections: false,
                    retry_delay: None,
                    retry_pending: false,
                },
            );
        }
        app.config.remote_hosts = vec![enabled.host.clone()];
        app.persist_config();
        assert!(app.config_save_pending());
        app.start_merged_remote_sessions();

        assert!(app.remote_config_refresh_pending);
        assert!(!app.remote_watcher_is_current(&disabled, 5));
        assert!(!app.remote_watcher_is_current(&pending, 5));
        assert!(app.remote_watcher_is_current(&enabled, 5));
        assert_eq!(app.remote_session_watchers.len(), 1);
        assert_eq!(app.workspaces[app.active_ws].id, active_id);
        assert!(app
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.remote.as_ref())
            .all(|remote| remote.host == enabled.host));
        assert!(matches!(
            disabled_input.try_recv(),
            Ok(ClientMessage::Detach)
        ));
        assert!(matches!(
            enabled_input.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        // Even with a second save coalesced behind the first, revoking the last
        // host is immediate and does not start discovery against stale disk data.
        app.config.remote_hosts.clear();
        app.persist_config();
        app.start_merged_remote_sessions();
        assert!(app.remote_session_watchers.is_empty());
        assert!(app
            .workspaces
            .iter()
            .all(|workspace| workspace.remote.is_none()));
        assert!(matches!(
            enabled_input.try_recv(),
            Ok(ClientMessage::Detach)
        ));
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
        let automation = json!({
            "name":"Owner task", "trigger":{"kind":"once","at_utc":4_000_000_000_u64},
            "task":{"title":"Check", "prompt":"Check", "agent_id":"codex",
                "workspace_id":app.workspaces[app.active_ws].id,
                "mode":"workspace", "access":"workspace"}
        });
        assert_eq!(
            app.dispatch("automation.create", &automation)
                .unwrap_err()
                .0,
            "remote_workspace"
        );
    }
}
