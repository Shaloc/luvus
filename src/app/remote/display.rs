//! Session-owned display bridges. Workspace views retain their identity and
//! cached frames; transport lifetime and switch tickets belong to the session.
use super::*;

pub(in crate::app) struct SessionDisplay {
    pub id: PaneId,
    pub generation: u64,
    pub target: RemoteSession,
    pub display: RemoteDisplay,
    pub input: Option<Arc<RemoteInput>>,
    pub epoch: u64,
    pub selected: Option<PaneId>,
    retry: Duration,
}

impl Drop for SessionDisplay {
    fn drop(&mut self) {
        if let Some(input) = &self.input {
            let _ = input.send(ClientMessage::Detach);
        }
    }
}

pub(in crate::app) struct PendingWorkspaceSwitch {
    pub pane: PaneId,
    pub connection: PaneId,
    pub generation: u64,
    pub epoch: u64,
    source: Option<String>,
    size: (u16, u16),
    pub commands: Vec<ClientMessage>,
    _deadline: crate::session::remote::ConnectionDeadline,
}

impl App {
    pub(super) fn ensure_session_display(
        &mut self,
        target: &RemoteSession,
        display: &RemoteDisplay,
    ) {
        if !display.session_display || !self.views.values().any(|view|
            matches!(view, ViewKind::Remote(view) if view.target.host == target.host && view.target.session == target.session)) {
            return;
        }
        let key = target.canonical_name();
        if !self.remote_session_displays.contains_key(&key) {
            let id = PaneId::alloc();
            let link = SessionDisplay {
                id,
                generation: 1,
                target: target.clone(),
                display: display.clone(),
                input: None,
                epoch: 0,
                selected: None,
                retry: REMOTE_RETRY_INITIAL,
            };
            spawn_projection(
                id,
                link.generation,
                RemoteWorkspaceRef {
                    host: target.host.clone(),
                    session: target.session.clone(),
                    workspace_id: String::new(),
                },
                display.clone(),
                Arc::new(AtomicBool::new(true)),
                self.app_tx.clone(),
                self.remote_connection_scope(target),
                Duration::ZERO,
            );
            self.remote_session_displays.insert(key.clone(), link);
        }
        let input = self.remote_session_displays[&key].input.clone();
        for view in self.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                if view.target.host == target.host && view.target.session == target.session {
                    view.projection.display = display.clone();
                    view.input.clone_from(&input);
                }
            }
        }
    }

    pub(super) fn session_display_ready(
        &mut self,
        id: PaneId,
        generation: u64,
        input: Arc<RemoteInput>,
    ) -> bool {
        let Some(link) = self
            .remote_session_displays
            .values_mut()
            .find(|link| link.id == id)
        else {
            return false;
        };
        if link.generation != generation {
            return true;
        }
        link.input = Some(input.clone());
        link.retry = REMOTE_RETRY_INITIAL;
        for view in self.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                if view.target.host == link.target.host
                    && view.target.session == link.target.session
                {
                    view.input = Some(input.clone());
                    view.projection.active = false;
                }
            }
        }
        if let Some(pending) = &self.pending_workspace_switch {
            if pending.connection == id {
                if let Some((index, _)) = self.pane_location(pending.pane) {
                    // Force a new ticket now that the bridge has an input sender.
                    self.pending_workspace_switch.as_mut().unwrap().generation = 0;
                    self.prepare_workspace_switch(index);
                }
            }
        } else if self
            .remote_workspace_view(self.active_ws)
            .is_some_and(|view| view.projection.display.session_display)
        {
            self.prepare_workspace_switch(self.active_ws);
        }
        true
    }

    pub(super) fn session_frame_route(
        &self,
        id: PaneId,
        generation: u64,
        state: Option<&protocol::ProjectionState>,
    ) -> Option<(PaneId, u64)> {
        let link = self
            .remote_session_displays
            .values()
            .find(|link| link.id == id && link.generation == generation)?;
        let pane = link.selected?;
        let ViewKind::Remote(view) = self.views.get(&pane)? else {
            return None;
        };
        (state?.workspace_id == view.target.workspace_id).then_some((pane, view.generation))
    }

    pub(super) fn session_display_closed(
        &mut self,
        id: PaneId,
        generation: u64,
        error: &str,
    ) -> bool {
        let Some(link) = self
            .remote_session_displays
            .values_mut()
            .find(|link| link.id == id)
        else {
            return false;
        };
        if link.generation != generation {
            return true;
        }
        link.input = None;
        link.selected = None;
        for view in self.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                if view.target.host == link.target.host
                    && view.target.session == link.target.session
                {
                    view.input = None;
                    view.state = RemoteViewState::Disconnected;
                    view.projection.active = false;
                    view.projection.frame_deadline = None;
                    view.error = Some(error.into());
                }
            }
        }
        let key = link.target.canonical_name();
        if !crate::session::remote::failure_needs_attention(error)
            && self.config.remote_hosts.contains(&link.target.host)
        {
            if let Some(watcher) = self
                .remote_session_watchers
                .get(&key)
                .filter(|watcher| !watcher.retry_pending)
            {
                let delay = link.retry;
                link.retry = (delay * 2).min(REMOTE_RETRY_MAX);
                link.generation = link.generation.wrapping_add(1);
                spawn_projection(
                    link.id,
                    link.generation,
                    RemoteWorkspaceRef {
                        host: link.target.host.clone(),
                        session: link.target.session.clone(),
                        workspace_id: String::new(),
                    },
                    link.display.clone(),
                    Arc::new(AtomicBool::new(true)),
                    self.app_tx.clone(),
                    watcher.scope.clone(),
                    delay,
                );
            }
        }
        if self
            .pending_workspace_switch
            .as_ref()
            .is_some_and(|p| p.connection == id)
        {
            self.cancel_workspace_switch();
            self.show_toast(error.to_string());
        }
        true
    }

    pub(in crate::app) fn cancel_workspace_switch(&mut self) {
        if let Some(pending) = self.pending_workspace_switch.take() {
            if let Some(input) = self
                .remote_session_displays
                .values()
                .find(|link| link.id == pending.connection && link.generation == pending.generation)
                .and_then(|link| link.input.as_ref())
            {
                let _ = input.send(ClientMessage::CancelWorkspace {
                    epoch: pending.epoch,
                });
            }
        }
    }

    pub(in crate::app) fn prepare_workspace_switch(&mut self, index: usize) -> bool {
        let Some(view) = self.remote_workspace_view(index) else {
            return false;
        };
        if !view.projection.display.session_display {
            return false;
        }
        let pane = self.workspaces[index].tabs[0].layout.focus;
        let workspace_id = view.target.workspace_id.clone();
        let key = RemoteSession {
            host: view.target.host.clone(),
            session: view.target.session.clone(),
        }
        .canonical_name();
        let Some(link) = self.remote_session_displays.get(&key) else {
            return false;
        };
        if index == self.active_ws
            && link.selected == Some(pane)
            && view.projection.active
            && view.state == RemoteViewState::Ready
        {
            return false;
        }
        let rect = self.remote_workspace_rect(index);
        let size = (rect.width.max(1), rect.height.max(1));
        if self
            .pending_workspace_switch
            .as_ref()
            .is_some_and(|p| p.pane == pane && p.size == size && p.generation == link.generation)
        {
            return true;
        }
        let usable_source = link
            .selected
            .and_then(|pane| self.views.get(&pane))
            .is_some_and(|view| {
                matches!(view, ViewKind::Remote(view)
                if view.projection.active && view.state == RemoteViewState::Ready)
            });
        let commands = self
            .pending_workspace_switch
            .as_mut()
            .filter(|p| p.pane == pane)
            .map(|p| std::mem::take(&mut p.commands))
            .unwrap_or_default();
        self.cancel_workspace_switch();
        let pixels = self.workspace_cell_pixels(self.active_ws).unwrap_or((0, 0));
        let graphics = self
            .workspaces
            .get(self.active_ws)
            .is_some_and(|ws| ws.graphics_enabled);
        let link = self.remote_session_displays.get_mut(&key).unwrap();
        link.epoch = link.epoch.saturating_add(1);
        let (connection, generation, epoch) = (link.id, link.generation, link.epoch);
        let tx = self.app_tx.clone();
        // A first frame (including after reconnect) proves this bridge works.
        // Its absence must retire the silent transport, not repeatedly prepare
        // on the same stuck pipe. A switch away from a usable surface can fail
        // locally while retaining that surface and its input.
        let deadline = if let Some(input) = link.input.as_ref().filter(|_| !usable_source) {
            input.expect_frame(REMOTE_RESPONSE_TIMEOUT)
        } else {
            crate::session::remote::ConnectionDeadline::on_expire(
                REMOTE_RESPONSE_TIMEOUT,
                move || {
                    let _ = tx.send(AppEvent::RemoteWorkspacePreparationFailed {
                        pane: connection,
                        generation,
                        epoch,
                        error: "remote workspace preparation timed out".into(),
                    });
                },
            )
        };
        self.pending_workspace_switch = Some(PendingWorkspaceSwitch {
            pane,
            connection,
            generation,
            epoch,
            size,
            source: self.workspaces.get(self.active_ws).map(|ws| ws.id.clone()),
            commands,
            _deadline: deadline,
        });
        if let Some(input) = &link.input {
            let result = input
                .send(ClientMessage::CellPixels {
                    cell_width: pixels.0,
                    cell_height: pixels.1,
                })
                .and_then(|()| {
                    input.send(ClientMessage::Graphics {
                        cell_width: if graphics { pixels.0 } else { 0 },
                        cell_height: if graphics { pixels.1 } else { 0 },
                    })
                })
                .and_then(|()| {
                    input.send(ClientMessage::PrepareWorkspace {
                        workspace_id,
                        epoch,
                        cols: size.0,
                        rows: size.1,
                    })
                });
            if let Err(error) = result {
                self.workspace_preparation_failed(connection, generation, epoch, error.to_string());
            }
        }
        true
    }

    pub(crate) fn workspace_preparation_failed(
        &mut self,
        connection: PaneId,
        generation: u64,
        epoch: u64,
        error: String,
    ) {
        if self.pending_workspace_switch.as_ref().is_some_and(|p| {
            p.connection == connection && p.generation == generation && p.epoch == epoch
        }) {
            self.cancel_workspace_switch();
            self.show_toast(error);
        } else if let Some(pane) = self
            .remote_session_displays
            .values()
            .find(|link| link.id == connection && link.generation == generation)
            .and_then(|link| link.selected)
        {
            if let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) {
                if view.projection.epoch == epoch && view.projection.active {
                    view.projection.active = false;
                    view.state = RemoteViewState::Disconnected;
                    view.error = Some(error.clone());
                    self.show_toast(error);
                }
            }
        }
    }

    pub(crate) fn apply_prepared_workspace(
        &mut self,
        connection: PaneId,
        generation: u64,
        state: protocol::ProjectionState,
        frame: FrameData,
        graphics: Vec<crate::terminal::graphics::Graphic>,
    ) {
        let Some(pending) = &self.pending_workspace_switch else {
            return;
        };
        if pending.connection != connection
            || pending.generation != generation
            || pending.epoch != state.epoch
            || pending.source != self.workspaces.get(self.active_ws).map(|ws| ws.id.clone())
        {
            return;
        }
        let pane = pending.pane;
        let Some((index, _)) = self.pane_location(pane) else {
            self.cancel_workspace_switch();
            return;
        };
        let Some(view) = self.remote_workspace_view(index) else {
            return;
        };
        if state.workspace_id != view.target.workspace_id
            || Some(&state.server_generation) != view.projection.display.server_generation.as_ref()
        {
            return;
        }
        let rect = self.remote_workspace_rect(index);
        if pending.size != (rect.width.max(1), rect.height.max(1)) {
            self.prepare_workspace_switch(index);
            return;
        }
        if pending.size != (frame.width, frame.height) {
            return;
        }
        let Some(link) = self
            .remote_session_displays
            .values_mut()
            .find(|link| link.id == connection && link.generation == generation)
        else {
            return;
        };
        let Some(input) = link.input.clone() else {
            return;
        };
        if input
            .send(ClientMessage::CommitWorkspace { epoch: state.epoch })
            .is_err()
        {
            self.cancel_workspace_switch();
            return;
        }
        link.selected = Some(pane);
        let pending = self.pending_workspace_switch.take().unwrap();
        if let Some(previous) = self
            .remote_display_pane
            .filter(|previous| *previous != pane)
        {
            self.suspend_remote_display(previous);
        }
        if let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) {
            view.last_size = pending.size;
            view.projection.active = true;
            view.projection.epoch = state.epoch;
            view.projection.frame_state = Some(state);
            view.projection.frame_deadline = None;
            view.input = Some(input.clone());
            view.frame = Some(frame);
            view.graphics = graphics;
            view.state = RemoteViewState::Ready;
            view.error = None;
        }
        self.remote_display_pane = Some(pane);
        self.focus_workspace_now(index);
        self.remote_mouse_capture = None;
        for command in pending.commands {
            if input.send(command).is_err() {
                break;
            }
        }
    }

    pub(super) fn apply_session_effect(
        &mut self,
        connection: PaneId,
        generation: u64,
        workspace_id: &str,
        epoch: u64,
        effect: protocol::WorkspaceEffect,
    ) {
        let Some(link) = self
            .remote_session_displays
            .values()
            .find(|link| link.id == connection && link.generation == generation)
        else {
            return;
        };
        let Some(pane) = link.selected else {
            return;
        };
        let Some(ViewKind::Remote(view)) = self.views.get(&pane) else {
            return;
        };
        if !view.projection.active
            || view.projection.epoch != epoch
            || view.target.workspace_id != workspace_id
            || self.active_remote_pane() != Some(pane)
        {
            return;
        }
        let generation = view.generation;
        self.apply_remote_effect(match effect {
            protocol::WorkspaceEffect::Focus(workspace_id) => RemoteEffect::Workspace {
                pane,
                generation,
                workspace_id,
            },
            protocol::WorkspaceEffect::Session(name) => RemoteEffect::Session {
                pane,
                generation,
                name,
            },
            protocol::WorkspaceEffect::Detach => RemoteEffect::Detach { pane, generation },
            protocol::WorkspaceEffect::Clipboard(text) => RemoteEffect::Clipboard(text),
            protocol::WorkspaceEffect::OpenUrl(url) => RemoteEffect::OpenUrl(url),
        });
    }

    pub(super) fn suspend_remote_display(&mut self, pane: PaneId) {
        let Some(ViewKind::Remote(view)) = self.views.get_mut(&pane) else {
            return;
        };
        if !view.projection.display.projection || !view.projection.active {
            return;
        }
        view.projection.active = false;
        view.projection.frame_deadline = None;
        if !view.projection.display.session_display {
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
            return;
        }
        if let Some(link) = self
            .remote_session_displays
            .values_mut()
            .find(|link| link.selected == Some(pane))
        {
            link.epoch = link.epoch.saturating_add(1);
            view.projection.epoch = link.epoch;
            if let Some(input) = &link.input {
                let _ = input.send(ClientMessage::ProjectionInterest {
                    epoch: link.epoch,
                    active: false,
                    cols: view.last_size.0,
                    rows: view.last_size.1,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::remote::tests::{
        add_remote_workspace, remote_ui_app, set_presentable_projection,
    };
    use ratatui::{buffer::Buffer, layout::Rect};

    fn fixture() -> (App, PaneId, mpsc::Receiver<ClientMessage>) {
        let mut app = remote_ui_app();
        let (first, rx, _) = add_remote_workspace(&mut app);
        set_presentable_projection(&mut app, first, 1);
        let input = app.remote_workspace_view(1).unwrap().input.clone();
        for i in 2..=16 {
            let (pane, _, _) = add_remote_workspace(&mut app);
            let ws = &mut app.workspaces[i];
            ws.id = format!("projection-{i}");
            ws.remote.as_mut().unwrap().workspace_id = format!("remote-{i}");
            let ViewKind::Remote(view) = app.views.get_mut(&pane).unwrap() else {
                unreachable!()
            };
            view.target.workspace_id = format!("remote-{i}");
            view.input = input.clone();
        }
        for view in app.views.values_mut() {
            if let ViewKind::Remote(view) = view {
                view.projection.display = RemoteDisplay {
                    session_display: true,
                    projection: true,
                    graphics: true,
                    cell_pixels: true,
                    server_generation: Some("boot".into()),
                    ..Default::default()
                };
            }
        }
        app.active_ws = 1;
        let target = RemoteSession::new("dev-207", "api").unwrap();
        let id = PaneId::alloc();
        app.remote_session_displays.insert(
            target.canonical_name(),
            SessionDisplay {
                id,
                generation: 1,
                target,
                display: app
                    .remote_workspace_view(1)
                    .unwrap()
                    .projection
                    .display
                    .clone(),
                input,
                epoch: 1,
                selected: Some(first),
                retry: REMOTE_RETRY_INITIAL,
            },
        );
        (app, id, rx)
    }

    fn response(app: &App) -> (protocol::ProjectionState, FrameData) {
        let pending = app.pending_workspace_switch.as_ref().unwrap();
        let ViewKind::Remote(view) = &app.views[&pending.pane] else {
            unreachable!()
        };
        let size = pending.size;
        (
            protocol::ProjectionState {
                workspace_id: view.target.workspace_id.clone(),
                server_generation: "boot".into(),
                epoch: pending.epoch,
                event_sequence: 1,
                focused_pane: Some("42".into()),
            },
            protocol::frame_from_buffer(
                &Buffer::empty(Rect::new(0, 0, size.0, size.1)),
                None,
                false,
            ),
        )
    }

    #[test]
    fn session_display_switch_keeps_old_input_and_rejects_stale_frames() {
        let _env = crate::persist::test_env("session-display-switch");
        let (mut app, id, rx) = fixture();
        app.focus_workspace(2);
        let (state, frame) = response(&app);
        assert_eq!(app.active_ws, 1);
        assert!(app.send_active_remote(ClientMessage::Paste("old input".into())));
        assert!(rx
            .try_iter()
            .any(|m| matches!(m, ClientMessage::Paste(text) if text == "old input")));
        let mut wrong_workspace = state.clone();
        wrong_workspace.workspace_id = "wrong".into();
        let mut old_epoch = state.clone();
        old_epoch.epoch = 0;
        let mut wrong_owner = state.clone();
        wrong_owner.server_generation = "previous boot".into();
        for (generation, invalid) in [
            (0, state.clone()),
            (1, wrong_workspace),
            (1, old_epoch),
            (1, wrong_owner),
        ] {
            app.apply_prepared_workspace(id, generation, invalid, frame.clone(), vec![]);
            assert_eq!(app.active_ws, 1);
        }
        app.focus_workspace(3);
        app.apply_prepared_workspace(id, 1, state, frame, vec![]);
        assert_eq!(app.active_ws, 1);
        let (state, frame) = response(&app);
        app.apply_prepared_workspace(id, 1, state.clone(), frame, vec![]);
        assert_eq!(app.active_ws, 3);
        assert!(!app.remote_workspace_view(1).unwrap().projection.active);
        assert!(app.pending_workspace_switch.is_none());
        assert!(app.send_active_remote(ClientMessage::Paste("new input".into())));
        let messages: Vec<_> = rx.try_iter().collect();
        let commit = messages
            .iter()
            .position(
                |m| matches!(m, ClientMessage::CommitWorkspace {epoch} if *epoch == state.epoch),
            )
            .unwrap();
        let input = messages
            .iter()
            .position(|m| matches!(m, ClientMessage::Paste(text) if text == "new input"))
            .unwrap();
        assert!(commit < input);
        assert_eq!(app.remote_session_displays.len(), 1);
        let shared = app
            .remote_workspace_view(1)
            .unwrap()
            .input
            .as_ref()
            .unwrap();
        assert!((1..=16).all(|i| Arc::ptr_eq(
            shared,
            app.remote_workspace_view(i)
                .unwrap()
                .input
                .as_ref()
                .unwrap()
        )));
    }

    #[test]
    fn session_display_handoff_suspends_other_owner_only_after_target_frame() {
        let _env = crate::persist::test_env("session-display-mixed-owners");
        for shared in [false, true] {
            let (mut app, id, _rx) = fixture();
            let first = app.active_remote_pane().unwrap();
            let (input, old_rx) = mpsc::channel();
            let input = Arc::new(RemoteInput::from(input));
            app.workspaces[1].remote.as_mut().unwrap().host = "old-owner".into();
            let ViewKind::Remote(view) = app.views.get_mut(&first).unwrap() else {
                unreachable!()
            };
            view.target.host = "old-owner".into();
            view.input = Some(input.clone());
            view.projection.display.session_display = shared;
            let display = view.projection.display.clone();
            app.remote_session_displays
                .values_mut()
                .next()
                .unwrap()
                .selected = None;
            if shared {
                let target = RemoteSession::new("old-owner", "api").unwrap();
                app.remote_session_displays.insert(
                    target.canonical_name(),
                    SessionDisplay {
                        id: PaneId::alloc(),
                        generation: 1,
                        target,
                        display,
                        input: Some(input),
                        epoch: 1,
                        selected: Some(first),
                        retry: REMOTE_RETRY_INITIAL,
                    },
                );
            }
            app.focus_workspace(2);
            assert!(
                old_rx.try_recv().is_err(),
                "source is still usable during preparation"
            );
            assert!(app.remote_workspace_view(1).unwrap().projection.active);
            let (state, frame) = response(&app);
            app.apply_prepared_workspace(id, 1, state, frame, vec![]);
            assert_eq!(app.active_ws, 2);
            assert!(matches!(
                old_rx.try_recv().unwrap(),
                ClientMessage::ProjectionInterest { active: false, .. }
            ));
            assert!(!app.remote_workspace_view(1).unwrap().projection.active);
        }
    }

    #[test]
    fn session_display_resize_timeout_and_effects_are_fenced() {
        let _env = crate::persist::test_env("session-display-fences");
        let (mut app, id, rx) = fixture();
        app.focus_workspace(2);
        assert!(app.send_workspace_remote(2, ClientMessage::Command("open_git".into())));
        let (old, frame) = response(&app);
        app.last_main_area.height += 1;
        app.apply_prepared_workspace(id, 1, old.clone(), frame, vec![]);
        assert_eq!(app.active_ws, 1);
        assert_eq!(
            app.pending_workspace_switch
                .as_ref()
                .unwrap()
                .commands
                .len(),
            1
        );
        assert!(app.pending_workspace_switch.as_ref().unwrap().epoch > old.epoch);
        app.workspace_preparation_failed(id, 1, old.epoch, "stale failure".into());
        assert!(app.pending_workspace_switch.is_some());
        let epoch = app.pending_workspace_switch.as_ref().unwrap().epoch;
        app.workspace_preparation_failed(id, 1, epoch, "timeout".into());
        assert!(app.pending_workspace_switch.is_none());
        assert_eq!(app.active_ws, 1);
        assert!(app.send_active_remote(ClientMessage::Command("old still usable".into())));
        app.focus_workspace(2);
        let (state, frame) = response(&app);
        app.apply_prepared_workspace(id, 1, state.clone(), frame, vec![]);
        app.apply_session_effect(
            id,
            1,
            "workspace_remote",
            1,
            protocol::WorkspaceEffect::Clipboard("stale".into()),
        );
        assert!(app.pending_clipboard.is_none());
        app.apply_session_effect(
            id,
            1,
            &state.workspace_id,
            state.epoch,
            protocol::WorkspaceEffect::Clipboard("current".into()),
        );
        assert_eq!(app.pending_clipboard.as_deref(), Some("current"));
        app.workspace_preparation_failed(id, 1, state.epoch, "closed before commit".into());
        assert!(!app.send_active_remote(ClientMessage::Paste("must not reach old owner".into())));
        assert!(!rx
            .try_iter()
            .any(|m| matches!(m, ClientMessage::Command(text) if text == "open_git")));
    }
}
