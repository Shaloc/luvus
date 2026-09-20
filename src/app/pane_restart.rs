//! Replace one PTY while keeping its layout slot. Native resume uses the same
//! trusted session identity and launch-flag policy as session restoration.

use super::*;

impl App {
    pub(crate) fn restart_pane(&mut self, id: PaneId) -> Result<PaneId, String> {
        let (workspace, tab) = self.pane_location(id).ok_or("pane not found")?;
        if self.module_panes.contains_key(&id) {
            return Err("module panes must be reopened through their module".into());
        }
        let pane = self
            .panes
            .get(&id)
            .ok_or("only terminal panes can restart")?;
        let cwd = pane.cwd.clone();
        let (cols, rows) = pane.size();
        let session = self
            .status
            .get(&id)
            .and_then(|status| status.agent_session.clone());
        let resume = session.as_ref().and_then(|session| {
            // A duplicate session report must not relaunch a second writer.
            if self.status.iter().any(|(other, status)| {
                *other != id
                    && status.agent_session.as_ref().is_some_and(|other| {
                        other.agent == session.agent && other.session_id == session.session_id
                    })
            }) {
                return None;
            }
            let launch = self
                .proc_commands
                .get(&id)
                .and_then(|commands| self.manifests.launch_args_for(commands, &session.agent));
            crate::agent::resume_for(
                &session.agent,
                &session.session_id,
                launch.as_deref(),
                self.config.resume_launch_flags,
            )
        });
        let name = self.agent_name_for(id).map(str::to_owned);
        let pinned = self.pinned_agents.contains(&id);
        let zoomed = self.zoomed;
        let replacement = PaneId::alloc();
        // A new ID fences late PtyExit/PtyReady events from the old terminal.
        // Replace the leaf before close_pane so all its ownership cleanup runs
        // without removing the layout slot or the enclosing tab/workspace.
        self.workspaces[workspace].tabs[tab]
            .layout
            .replace_pane(id, replacement);
        self.close_pane(id);

        let shell = crate::platform::resolve_shell(&self.config.shell);
        let pane = Pane::spawn_resume_deferred(
            replacement,
            cols,
            rows,
            cwd,
            self.app_tx.clone(),
            &shell,
            resume.as_deref(),
            self.config.scrollback_bytes(),
            self.pane_appearance,
            self.workspace_cell_pixels(workspace),
        );
        let mut status = PaneStatus::new(pane.command.clone());
        if resume.is_some() {
            status.agent = session.as_ref().unwrap().agent.clone();
            status.agent_session = session;
        }
        self.panes.insert(replacement, pane);
        self.status.insert(replacement, status);
        if let Some(name) = name {
            self.agent_names.insert(name, replacement);
        }
        if pinned {
            self.pinned_agents.insert(replacement);
        }
        self.zoomed = zoomed;
        self.force_redraw = true;
        crate::logging::event(
            crate::logging::EventKind::PaneOpen,
            &[
                crate::logging::Field::PaneId(u64::from(replacement.0)),
                crate::logging::Field::SpawnKind(if resume.is_some() {
                    crate::logging::SpawnKind::Resume
                } else {
                    crate::logging::SpawnKind::Shell
                }),
            ],
        );
        self.emit_event("pane.created", json!({"pane":replacement.0.to_string()}));
        Ok(replacement)
    }

    pub(crate) fn confirm_pane_restart(&mut self) {
        if let Some(id) = self.pane_restart_confirm.take() {
            if let Err(error) = self.restart_pane(id) {
                self.show_toast(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;

    fn tap(app: &mut App, rect: Rect) {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        }));
    }

    #[test]
    fn pane_restart_button_confirms_and_recovers_only_failed_pane() {
        let _env = crate::persist::test_env("pane-restart-button");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let old = app.layout().focus;
        let sibling = app.split_pane(old, Axis::Row, false).unwrap();
        let sibling_engine = app.panes[&sibling].engine.clone();
        let cwd = app.panes[&old].cwd.clone();
        app.agent_names.insert("kept-name".into(), old);
        app.pinned_agents.insert(old);
        let engine = app.panes[&old].engine.clone();
        assert!(std::thread::spawn(move || {
            let _guard = engine.lock().unwrap();
            panic!("injected terminal failure");
        })
        .join()
        .is_err());

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let button = app.pane_restart_rect.unwrap();
        assert_eq!(
            terminal.backend().buffer()[(button.x + 1, button.y)].symbol(),
            "↻"
        );
        let area = Rect::new(0, 0, 120, 40);
        let before = app.layout().pane_rect(area, old).unwrap();
        tap(&mut app, button);
        assert_eq!(app.pane_restart_confirm, Some(old));
        assert!(
            app.panes.contains_key(&old),
            "opening the prompt does not stop anything"
        );
        assert!(!crate::ui::retained_pty_eligible(&app));
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(app.panes.contains_key(&old));
        tap(&mut app, button);
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let confirm = app.modal_commit_rect.unwrap();
        tap(&mut app, confirm);
        let new = app.layout().focus;
        assert_ne!(new, old);
        assert_eq!(app.layout().pane_rect(area, new), Some(before));
        assert_eq!(app.layout().len(), 2);
        assert_eq!(app.panes[&new].cwd, cwd);
        assert_eq!(app.agent_names["kept-name"], new);
        assert!(app.pinned_agents.contains(&new));
        assert!(app.panes[&new].engine.lock().is_ok());
        assert!(Arc::ptr_eq(&app.panes[&sibling].engine, &sibling_engine));
        app.handle_event(AppEvent::PtyExit(old));
        assert!(
            app.panes.contains_key(&new),
            "late old exit cannot close replacement"
        );
        assert!(app.panes.contains_key(&sibling));
    }

    #[test]
    fn pane_restart_preserves_inactive_tab_and_native_identity() {
        let _env = crate::persist::test_env("pane-restart-inactive");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let old = app.layout().focus;
        app.status.get_mut(&old).unwrap().agent_session = Some(AgentSession {
            agent: "codex".into(),
            session_id: "known-native-session".into(),
        });
        #[cfg(unix)]
        {
            app.config.shell = "/bin/cat".into();
        }
        app.new_tab();
        let other = crate::persist::config_dir().join("another-workspace");
        std::fs::create_dir_all(&other).unwrap();
        assert!(app.create_workspace_at_with_focus(other, true));
        let active = app.layout().focus;
        let new = app.restart_pane(old).unwrap();
        assert_eq!(app.layout().focus, active);
        assert_eq!(app.active_ws, 1);
        assert_eq!(app.workspaces[0].active_tab, 1);
        assert_eq!(app.pane_location(new), Some((0, 0)));
        assert_eq!(
            app.status[&new].agent_session.as_ref().unwrap().session_id,
            "known-native-session"
        );
        assert!(app.panes.contains_key(&active));
    }

    #[test]
    fn pane_restart_api_validates_before_mutation_and_returns_new_identity() {
        let _env = crate::persist::test_env("pane-restart-api");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let old = app.layout().focus;
        for params in [
            json!({"pane":old.0, "all":true}),
            json!({"pane":"bad"}),
            json!({"pane":u32::MAX}),
        ] {
            assert!(app.dispatch("pane.restart", &params).is_err());
            assert!(app.panes.contains_key(&old));
        }
        let result = app
            .dispatch("pane.restart", &json!({"pane":old.0}))
            .unwrap();
        assert_eq!(result["previous_pane"], old.0.to_string());
        assert_eq!(result["tab"], "1");
        assert_ne!(result["pane"], old.0.to_string());
        app.workspaces.clear();
        assert!(app.dispatch("pane.restart", &json!({})).is_err());
    }

    #[test]
    fn pane_restart_button_is_available_in_lone_and_zoomed_headers() {
        let _env = crate::persist::test_env("pane-restart-headers");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for zoomed in [false, true] {
            app.zoomed = zoomed;
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let button = app.pane_restart_rect.unwrap();
            assert_eq!(
                terminal.backend().buffer()[(button.x + 1, button.y)].symbol(),
                "↻"
            );
            if let Some(zoom) = app.pane_zoom_rect {
                assert!(!button.intersects(zoom));
            }
        }
    }
}
