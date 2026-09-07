//! Agent-dock projection for managed remotes. The owner keeps native history,
//! automation definitions and pin state; this module only projects their cached
//! metadata through the existing snapshot/events bridge and reuses owner actions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{AgentMenuItem, AgentTarget, App, ViewKind};
use crate::ids::PaneId;
use crate::ipc::protocol::ClientMessage;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHistoryRow {
    pub key: [u8; 32],
    pub agent: String,
    pub cwd: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::remote::tests::{add_remote_workspace, remote_ui_app};
    use crate::app::remote::{RemoteAgentMeta, RemoteView};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use serde_json::json;

    fn remote_view(app: &mut App, pane: PaneId) -> &mut RemoteView {
        let Some(ViewKind::Remote(view)) = app.views.get_mut(&pane) else {
            panic!("remote fixture")
        };
        view
    }

    fn history(cwd: std::path::PathBuf, id: &str) -> crate::agent::SessionInfo {
        crate::agent::SessionInfo {
            agent: "codex".into(),
            session_id: id.into(),
            cwd,
            updated: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    fn agent(pane: u32, pinned: bool) -> RemoteAgentMeta {
        RemoteAgentMeta {
            pane: pane.to_string(),
            agent: "codex".into(),
            state: crate::ui::theme::State::Blocked,
            tab: 1,
            focused: false,
            name: None,
            session: None,
            terminal_id: None,
            cwd: "/srv/api".into(),
            pinned,
        }
    }

    fn render(app: &mut App) -> String {
        render_buffer(app)
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn render_buffer(app: &mut App) -> ratatui::buffer::Buffer {
        let area = ratatui::layout::Rect::new(0, 0, 160, 60);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        crate::ui::render_into(&mut crate::ui::RenderTarget::new(&mut buffer, area), app);
        buffer
    }

    #[test]
    fn remote_agent_mouse_selection_clears_keyboard_cursor_and_highlights_owner_identity() {
        use crate::event::AppEvent;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let _env = crate::persist::test_env("remote-agent-selected-highlight");
        for paths in [false, true] {
            let mut app = remote_ui_app();
            app.config.layout.agent_paths = paths;
            app.agents_active_only = true;
            let (view, input, _) = add_remote_workspace(&mut app);
            remote_view(&mut app, view).agents = vec![agent(7, false), agent(8, false)];
            remote_view(&mut app, view).agents[0].focused = true;
            app.focus_agents_dock();
            render(&mut app);
            let hit = app
                .remote_agent_rects
                .iter()
                .find(|(_, pane, _)| pane == "8")
                .unwrap()
                .2;
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x + 3,
                row: hit.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert_eq!(
                app.sidebar_focus, None,
                "mouse selection must retire the old keyboard row"
            );
            assert!(
                matches!(input.try_recv().unwrap(), ClientMessage::Command(command)
                if command == "remote_agent_focus 8")
            );
            // Apply the owner's acknowledged focus, then reorder equal-name
            // agents: the highlight must follow pane identity, not list index.
            for agent in &mut remote_view(&mut app, view).agents {
                agent.focused = agent.pane == "8";
            }
            for reorder in [false, true] {
                if reorder {
                    remote_view(&mut app, view).agents.swap(0, 1);
                }
                let buffer = render_buffer(&mut app);
                for (_, pane, rect) in &app.remote_agent_rects {
                    assert_eq!(
                        buffer[(rect.x + 3, rect.y)].bg == app.theme.sel_bg,
                        pane == "8",
                        "highlight must identify owner pane {pane}"
                    );
                }
            }
            app.active_ws = 0;
            let buffer = render_buffer(&mut app);
            for (_, _, rect) in &app.remote_agent_rects {
                assert_ne!(buffer[(rect.x + 3, rect.y)].bg, app.theme.sel_bg);
            }
        }
    }

    #[test]
    fn local_history_resume_target_never_injects_into_remote_workspace() {
        let _env = crate::persist::test_env("remote-history-local-boundary");
        let mut app = remote_ui_app();
        let (_, _input, remote_cwd) = add_remote_workspace(&mut app);
        app.workspaces[0].cwd = remote_cwd.clone();
        for own_workspace in [false, true] {
            app.config.layout.resume_in_new_workspace = own_workspace;
            assert_eq!(app.resume_workspace_target(&remote_cwd), Some(0));
            assert_eq!(
                app.resume_workspace_target(std::path::Path::new("/unopened/project")),
                None
            );
        }
        assert!(
            app.panes.is_empty(),
            "fixture never launches a resume process"
        );
    }

    #[test]
    fn remote_dock_filter_and_scope_shortcuts_stay_local() {
        let _env = crate::persist::test_env("remote-agent-filter-owner");
        let mut app = remote_ui_app();
        let (_, input, _) = add_remote_workspace(&mut app);
        app.keymap.insert("a".into(), crate::app::Cmd::ToggleAgents);
        app.keymap
            .insert("A".into(), crate::app::Cmd::ToggleAgentScope);
        let active_before = app.agents_active_only;
        app.mode = crate::app::Mode::Prefix;
        app.handle_active_remote_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(
            app.sidebar_focus,
            Some(crate::app::SidebarListFocus::Agents)
        );
        assert_eq!(app.agents_active_only, active_before);
        app.handle_event(crate::event::AppEvent::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.agents_active_only, !active_before);
        let scoped_before = app.agents_this_workspace;
        app.mode = crate::app::Mode::Prefix;
        app.handle_active_remote_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(app.agents_this_workspace, !scoped_before);
        assert!(
            input.try_recv().is_err(),
            "outer filter shortcuts must not mutate hidden owner settings"
        );
    }

    #[test]
    fn next_attention_crosses_local_and_remote_workspace_boundaries() {
        let _env = crate::persist::test_env("remote-agent-attention");
        let mut app = remote_ui_app();
        let (view, input, _) = add_remote_workspace(&mut app);
        remote_view(&mut app, view).agents = vec![agent(7, false)];
        app.active_ws = 0;
        app.focus_next_attention();
        assert_eq!(app.active_ws, 1);
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == "remote_agent_focus 7")
        );
        let local = app.workspaces[0].tabs[0].layout.focus;
        let mut status = crate::app::PaneStatus::new("codex".into());
        status.state = crate::ui::theme::State::Blocked;
        app.status.insert(local, status);
        remote_view(&mut app, view).agents[0].focused = true;
        app.focus_next_attention();
        assert_eq!(app.active_ws, 0);
        assert!(app.panes.is_empty());
    }

    #[test]
    fn remote_agent_menu_scope_is_local_and_pin_rename_close_are_owner_commands() {
        let _env = crate::persist::test_env("remote-agent-menus");
        let mut app = remote_ui_app();
        let (view, input, _) = add_remote_workspace(&mut app);
        remote_view(&mut app, view).agents = vec![agent(7, false)];
        let target = AgentTarget::RemoteLive {
            view,
            pane: PaneId(7),
        };
        app.open_agent_menu(target.clone(), 2, 2);
        assert!(app
            .agent_menu_items(target.clone())
            .contains(&AgentMenuItem::Pin));
        let scoped_before = app.agents_this_workspace;
        app.agent_menu_action(AgentMenuItem::ToggleWorkspaceScope);
        assert_eq!(app.agents_this_workspace, !scoped_before);
        assert!(input.try_recv().is_err());
        let paths_before = app.config.layout.agent_paths;
        app.open_agent_menu(target.clone(), 2, 2);
        app.agent_menu_action(AgentMenuItem::TogglePath);
        assert_eq!(app.config.layout.agent_paths, !paths_before);
        assert!(
            input.try_recv().is_err(),
            "path visibility belongs to the display"
        );
        for (item, expected) in [
            (AgentMenuItem::Pin, "agent_pin 7"),
            (AgentMenuItem::Unpin, "agent_unpin 7"),
            (AgentMenuItem::RenamePane, "agent_rename 7"),
            (AgentMenuItem::Close, "agent_close 7"),
        ] {
            app.open_agent_menu(target.clone(), 2, 2);
            app.agent_menu_action(item);
            assert!(
                matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == expected)
            );
        }
        assert!(
            app.pinned_agents.is_empty(),
            "remote pin state is owned remotely"
        );
        assert!(app.pane_rename.is_none());
        remote_view(&mut app, view).agents[0].pinned = true;
        app.open_agent_menu(target.clone(), 2, 2);
        assert!(app.agent_menu_items(target).contains(&AgentMenuItem::Unpin));
        remote_view(&mut app, view).agents.clear();
        app.agent_menu_action(AgentMenuItem::Close);
        assert!(
            input.try_recv().is_err(),
            "stale row must not close another owner pane"
        );
    }

    #[test]
    fn owner_action_menus_preserve_sidebar_anchor_in_projection_coordinates() {
        let _env = crate::persist::test_env("remote-owner-menu-anchor");
        let mut app = remote_ui_app();
        let (view, input, _) = add_remote_workspace(&mut app);
        remote_view(&mut app, view).agents = vec![agent(7, false)];
        app.pane_content_rects = vec![(view, Rect::new(30, 3, 60, 20))];
        let workspace = app.pane_location(view).unwrap().0;
        app.open_ws_menu(workspace, 12, 17);
        app.ws_menu_action(crate::app::WsMenuItem::OwnerModules);
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command)
            if command == "open_workspace_menu 0 14")
        );
        app.open_agent_menu(
            AgentTarget::RemoteLive {
                view,
                pane: PaneId(7),
            },
            95,
            30,
        );
        app.agent_menu_action(AgentMenuItem::OwnerActions);
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command)
            if command == "remote_agent_menu 7 59 19")
        );
        assert!(app.agent_menu.is_none());
    }

    #[test]
    fn owner_menu_anchor_for_inactive_workspace_uses_current_outer_chrome() {
        let _env = crate::persist::test_env("remote-inactive-owner-menu-anchor");
        let mut app = remote_ui_app();
        let (view, _, _) = add_remote_workspace(&mut app);
        app.pane_content_rects.clear();
        app.last_main_area = Rect::new(0, 0, 120, 29);
        app.left_seam = Some(Rect::new(29, 0, 1, 29));
        app.right_seam = Some(Rect::new(100, 0, 1, 29));
        let workspace = app.pane_location(view).unwrap().0;
        assert_eq!(app.remote_menu_anchor(workspace, (8, 17)), (0, 17));
        assert_eq!(app.remote_menu_anchor(workspace, (118, 27)), (69, 27));
    }

    #[test]
    fn owner_menu_commands_accept_optional_coordinates_and_reject_invalid_extras() {
        let _env = crate::persist::test_env("remote-owner-menu-coordinates-grammar");
        let mut owner = remote_ui_app();
        let pane = owner.layout().focus.0;
        for (command, expected) in [
            ("open_workspace_menu".to_string(), (2, 2)),
            ("open_workspace_menu 21 7".to_string(), (21, 7)),
        ] {
            owner.handle_event(crate::event::AppEvent::ClientCommand(command));
            assert_eq!(owner.ws_menu.take().unwrap().anchor, expected);
        }
        for (suffix, expected) in [("", (2, 2)), (" 21 7", (21, 7))] {
            owner.handle_event(crate::event::AppEvent::ClientCommand(format!(
                "remote_agent_menu {pane}{suffix}"
            )));
            assert_eq!(owner.agent_menu.take().unwrap().anchor, expected);
        }
        for suffix in [" 1", " -1 2", " 65536 2", " 1 2 3", " x 2"] {
            owner.handle_event(crate::event::AppEvent::ClientCommand(format!(
                "open_workspace_menu{suffix}"
            )));
            assert!(owner.ws_menu.is_none());
            owner.handle_event(crate::event::AppEvent::ClientCommand(format!(
                "remote_agent_menu {pane}{suffix}"
            )));
            assert!(owner.agent_menu.is_none());
        }
    }

    #[test]
    fn remote_history_keys_survive_reordering_and_reject_stale_rows() {
        let _env = crate::persist::test_env("remote-history-stable-action");
        let mut owner = remote_ui_app();
        let first = history(owner.ws().cwd.clone(), "first");
        let second = history(owner.ws().cwd.clone(), "second");
        let key = history_key(&first);
        owner.resumable = vec![second, first];
        let command = format!("agent_dismiss {}", crate::base64_encode(&key));
        assert!(command.len() <= 64);
        assert!(owner.handle_remote_agent_command(&command));
        assert_eq!(owner.resumable.len(), 1);
        assert_eq!(owner.resumable[0].session_id, "second");
        assert!(owner.handle_remote_agent_command(&command));
        assert_eq!(
            owner.resumable.len(),
            1,
            "stale identity is a no-op, not an index fallback"
        );
        assert!(owner.panes.is_empty());
    }

    #[test]
    fn remote_history_and_scheduled_rows_render_and_click_on_their_owner() {
        let _env = crate::persist::test_env("remote-agent-history-scheduled");
        let mut app = remote_ui_app();
        let (view, input, _) = add_remote_workspace(&mut app);
        let key = history_key(&history(
            std::path::PathBuf::from("/srv/api"),
            "native-session",
        ));
        let remote = remote_view(&mut app, view);
        remote.history.push(AgentHistoryRow {
            key,
            agent: "codex".into(),
            cwd: "/srv/api".into(),
        });
        remote.scheduled.push(ScheduledAgentRow {
            id: "a1".into(),
            agent: "codex".into(),
            workspace_id: remote.target.workspace_id.clone(),
            deadline: 1_900_000_000,
            starting: false,
            target_state: None,
        });
        app.agents_active_only = false;
        app.agents_this_workspace = false;
        let text = render(&mut app);
        assert!(
            text.contains("resume") && text.contains("dev-207"),
            "{text}"
        );
        assert_eq!(app.remote_history_rects.len(), 1);
        assert_eq!(app.remote_automation_rects.len(), 1);
        assert!(app.session_rects.is_empty());
        assert!(app.automation_rects.is_empty());
        let history_rect = app.remote_history_rects[0].2;
        app.handle_event(crate::event::AppEvent::Mouse(
            ratatui::crossterm::event::MouseEvent {
                kind: ratatui::crossterm::event::MouseEventKind::Down(
                    ratatui::crossterm::event::MouseButton::Left,
                ),
                column: history_rect.x + 2,
                row: history_rect.y,
                modifiers: KeyModifiers::NONE,
            },
        ));
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == format!("agent_resume {}", crate::base64_encode(&key)))
        );
        let rect = app.remote_automation_rects[0].2;
        app.handle_event(crate::event::AppEvent::Mouse(
            ratatui::crossterm::event::MouseEvent {
                kind: ratatui::crossterm::event::MouseEventKind::Down(
                    ratatui::crossterm::event::MouseButton::Left,
                ),
                column: rect.x + 2,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
        ));
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == "automation_detail a1")
        );
        assert!(app.orch_detail.is_none());
        app.agents_active_only = true;
        render(&mut app);
        assert!(app.remote_history_rects.is_empty());
        assert_eq!(app.remote_automation_rects.len(), 1);
        app.active_ws = 0;
        app.agents_this_workspace = true;
        render(&mut app);
        assert!(app.remote_history_rects.is_empty() && app.remote_automation_rects.is_empty());
        assert!(app.panes.is_empty());
    }

    #[test]
    fn snapshot_projects_cached_history_schedules_and_legacy_defaults() {
        let _env = crate::persist::test_env("remote-agent-snapshot");
        let mut owner = remote_ui_app();
        owner
            .resumable
            .push(history(owner.ws().cwd.clone(), "native-session"));
        let automation: crate::automation::Automation = serde_json::from_value(json!({
            "id":"a1", "name":"planned", "enabled":true, "trigger":{"kind":"once", "at_utc":1_900_000_000},
            "task":{"title":"planned", "prompt":"fixture", "agent_id":"codex", "workspace_id":owner.ws().id,
                "mode":"workspace"}, "next_run_at":1_900_000_000, "created_at":1, "updated_at":1
        })).unwrap();
        owner.automation.automations.push(automation);
        owner
            .automation
            .active_target_states
            .insert("a1".into(), crate::automation::ActiveTargetState::Restoring);
        let snapshot = owner.runtime_snapshot();
        let parsed = crate::app::remote::parse_remote_snapshot(
            &json!({"result":snapshot}),
            crate::session::remote::RemoteBinaryLocation::Path,
        )
        .unwrap();
        assert_eq!(parsed.workspaces[0].history.len(), 1);
        assert_eq!(parsed.workspaces[0].scheduled[0].id, "a1");
        assert_eq!(
            parsed.workspaces[0].scheduled[0].target_state,
            Some(crate::automation::ActiveTargetState::Restoring)
        );
        let legacy_row: ScheduledAgentRow = serde_json::from_value(json!({
            "id":"old", "agent":"codex", "workspace_id":"w", "deadline":1,
            "starting":false
        }))
        .unwrap();
        assert_eq!(legacy_row.target_state, None);
        assert_eq!(
            parsed.workspaces[0].history[0].key,
            history_key(&owner.resumable[0])
        );
        owner.handle_remote_agent_command("automation_detail a1");
        assert_eq!(owner.orch_detail.as_deref(), Some("a1"));
        assert!(owner.panes.is_empty());
        let legacy = crate::app::remote::parse_remote_snapshot(
            &json!({"result":{"event_sequence":0,"workspaces":[{
                "id":"legacy", "name":"legacy", "cwd":"/srv/api", "tabs":[]
            }]}}),
            crate::session::remote::RemoteBinaryLocation::Path,
        )
        .unwrap();
        assert!(
            legacy.workspaces[0].history.is_empty() && legacy.workspaces[0].scheduled.is_empty()
        );
    }

    #[test]
    fn merged_agent_keyboard_targets_match_owner_rows_and_hidden_path_geometry() {
        use crate::app::{AgentDockTarget, PaneStatus, SidebarListFocus};
        use crate::event::AppEvent;
        let _env = crate::persist::test_env("merged-agent-keyboard");
        let mut app = remote_ui_app();
        let (view, input, _) = add_remote_workspace(&mut app);
        let local = app.workspaces[0].tabs[0].layout.focus;
        app.status.insert(local, PaneStatus::new("codex".into()));
        let local_history = history(app.workspaces[0].cwd.clone(), "local-history");
        app.resumable.push(local_history);
        let key = history_key(&history("/srv/api".into(), "remote-history"));
        let remote = remote_view(&mut app, view);
        let mut pinned = agent(7, true);
        pinned.focused = true;
        remote.agents = vec![pinned];
        remote.history.push(AgentHistoryRow {
            key,
            agent: "codex".into(),
            cwd: "/srv/api".into(),
        });
        remote.scheduled.push(ScheduledAgentRow {
            id: "remote-plan".into(),
            agent: "codex".into(),
            workspace_id: remote.target.workspace_id.clone(),
            deadline: 1_900_000_000,
            starting: false,
            target_state: Some(crate::automation::ActiveTargetState::NeedsRebind),
        });
        app.agents_active_only = false;
        app.agents_this_workspace = false;
        let expected = vec![
            AgentDockTarget::RemoteLive {
                view,
                pane: "7".into(),
            },
            AgentDockTarget::Live(local),
            AgentDockTarget::RemoteAutomation {
                view,
                id: "remote-plan".into(),
            },
            AgentDockTarget::Session(0),
            AgentDockTarget::RemoteSession { view, key },
        ];
        for paths in [true, false] {
            app.config.layout.agent_paths = paths;
            assert_eq!(app.agent_dock_targets(), expected);
            let text = render(&mut app);
            let stride = if paths { 2 } else { 1 };
            assert_eq!(app.remote_agent_rects[0].2.height, stride);
            assert_eq!(app.remote_automation_rects[0].2.height, stride);
            assert_eq!(app.remote_history_rects[0].2.height, stride);
            assert_eq!(app.session_rects[0].1.height, stride);
            assert!(text.contains(app.catalog.automation_needs_rebind), "{text}");
            assert!(text.contains("[dev-207]"), "{text}");
        }

        app.focus_agents_dock();
        assert_eq!(
            app.agent_cursor, 0,
            "focus follows the owner's focused agent"
        );
        let key_event = |code| AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
        app.handle_event(key_event(KeyCode::Down));
        assert_eq!(app.agent_cursor, 1);
        assert!(
            input.try_recv().is_err(),
            "list navigation must not reach the owner PTY"
        );
        app.handle_event(key_event(KeyCode::Enter));
        assert_eq!(app.active_ws, 0);
        assert_eq!(app.sidebar_focus, None);
        app.focus_agents_dock();
        app.handle_event(key_event(KeyCode::Home));
        app.handle_event(key_event(KeyCode::Char('a')));
        assert_eq!(
            app.agent_menu.as_ref().unwrap().target,
            AgentTarget::RemoteLive {
                view,
                pane: PaneId(7)
            }
        );
        app.handle_event(key_event(KeyCode::Esc));
        app.handle_event(key_event(KeyCode::Enter));
        assert_eq!(app.active_ws, 1);
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == "remote_agent_focus 7")
        );
        app.focus_agents_dock();
        app.agent_cursor = 2;
        app.handle_event(key_event(KeyCode::Enter));
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == "automation_detail remote-plan")
        );
        app.focus_agents_dock();
        app.handle_event(key_event(KeyCode::End));
        app.handle_event(key_event(KeyCode::Enter));
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == format!("agent_resume {}", crate::base64_encode(&key)))
        );
        assert!(app.panes.is_empty());

        app.active_ws = 0;
        app.agents_this_workspace = true;
        assert_eq!(
            app.agent_dock_targets(),
            vec![
                AgentDockTarget::Live(local),
                AgentDockTarget::Session(0),
                AgentDockTarget::RemoteElsewhere {
                    view,
                    pane: "7".into()
                },
            ]
        );
        app.focus_agents_dock();
        app.handle_event(key_event(KeyCode::End));
        app.handle_event(key_event(KeyCode::Enter));
        assert_eq!(app.active_ws, 1);
        assert_eq!(app.sidebar_focus, None);
        assert!(
            matches!(input.try_recv().unwrap(), ClientMessage::Command(command) if command == "remote_agent_focus 7")
        );
        assert_ne!(app.sidebar_focus, Some(SidebarListFocus::Agents));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledAgentRow {
    pub id: String,
    pub agent: String,
    pub workspace_id: String,
    pub deadline: u64,
    pub starting: bool,
    #[serde(default)]
    pub target_state: Option<crate::automation::ActiveTargetState>,
}

pub fn history_key(session: &crate::agent::SessionInfo) -> [u8; 32] {
    let mut hash = Sha256::new();
    for field in [
        session.agent.as_bytes(),
        session.session_id.as_bytes(),
        session.cwd.as_os_str().as_encoded_bytes(),
    ] {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    hash.finalize().into()
}

impl App {
    /// A native history entry always resumes locally, even when the outer
    /// workspace focus is currently on a remote projection with the same path.
    pub(crate) fn resume_workspace_target(&self, cwd: &std::path::Path) -> Option<usize> {
        if self.config.layout.resume_in_new_workspace
            || self
                .workspaces
                .get(self.active_ws)
                .is_some_and(|workspace| workspace.remote.is_some())
        {
            self.workspaces.iter().position(|workspace| {
                workspace.remote.is_none() && crate::platform::same_path(&workspace.cwd, cwd)
            })
        } else {
            (!self.workspaces.is_empty()).then_some(self.active_ws)
        }
    }
    /// Same lightweight scheduled rows for the native and remote AGENTS dock.
    /// No disk reads or new automation state: the existing ledger is authoritative.
    pub(crate) fn scheduled_agent_rows(&self) -> Vec<ScheduledAgentRow> {
        let mut rows: Vec<_> = self
            .automation
            .automations
            .iter()
            .filter_map(|automation| {
                if !automation.enabled
                    || !self.workspaces.iter().any(|workspace| {
                        workspace.remote.is_none() && workspace.id == automation.task.workspace_id
                    })
                {
                    return None;
                }
                let live = self
                    .automation
                    .runs
                    .iter()
                    .rev()
                    .find(|run| run.automation_id == automation.id && run.status.is_live());
                let pane_backed = live
                    .and_then(|run| run.task_id.as_deref())
                    .and_then(|task| self.orch.task(task))
                    .and_then(|task| task.assignee)
                    .is_some_and(|pane| self.panes.contains_key(&PaneId(pane)));
                if pane_backed
                    || live.is_some_and(|run| {
                        matches!(
                            run.status,
                            crate::automation::RunStatus::Running
                                | crate::automation::RunStatus::Review
                        )
                    })
                {
                    return None;
                }
                Some(ScheduledAgentRow {
                    id: automation.id.clone(),
                    agent: automation.task.agent_id.clone(),
                    workspace_id: automation.task.workspace_id.clone(),
                    deadline: live
                        .map(|run| run.scheduled_at)
                        .or(automation.next_run_at)?,
                    starting: live.is_some(),
                    target_state: self
                        .automation
                        .active_target_states
                        .get(&automation.id)
                        .copied(),
                })
            })
            .collect();
        rows.sort_by_key(|row| row.deadline);
        rows
    }

    /// Associate owner history with the longest open root. Unopened projects
    /// remain resumable from the first owner workspace, as in the native All list.
    pub(crate) fn agent_history_rows(&self, workspace: usize) -> Vec<AgentHistoryRow> {
        self.resumable
            .iter()
            .filter(|session| {
                self.workspaces
                    .iter()
                    .enumerate()
                    .filter(|(_, ws)| {
                        ws.remote.is_none() && crate::platform::is_subpath(&session.cwd, &ws.cwd)
                    })
                    .max_by_key(|(_, ws)| ws.cwd.as_os_str().len())
                    .map(|(index, _)| index)
                    .or_else(|| self.workspaces.iter().position(|ws| ws.remote.is_none()))
                    == Some(workspace)
            })
            .map(|session| AgentHistoryRow {
                key: history_key(session),
                agent: session.agent.clone(),
                cwd: session.cwd.display().to_string(),
            })
            .collect()
    }

    pub(crate) fn remote_agent_is_pinned(&self, view: PaneId, pane: PaneId) -> bool {
        matches!(self.views.get(&view), Some(ViewKind::Remote(view))
            if view.agents.iter().any(|agent| agent.pane == pane.0.to_string() && agent.pinned))
    }

    pub(crate) fn remote_agent_menu_action(
        &mut self,
        target: AgentTarget,
        item: AgentMenuItem,
    ) -> bool {
        if item == AgentMenuItem::TogglePath {
            return false; // Shared display setting, even on a remote row.
        }
        let (view, command) = match target {
            AgentTarget::RemoteLive { view, pane } => {
                let action = match item {
                    AgentMenuItem::RenamePane => "agent_rename",
                    AgentMenuItem::Close => "agent_close",
                    AgentMenuItem::Pin => "agent_pin",
                    AgentMenuItem::Unpin => "agent_unpin",
                    AgentMenuItem::OwnerActions => "remote_agent_menu",
                    _ => return true,
                };
                // A stale menu cannot act on an unrelated pane after topology changes.
                if !matches!(self.views.get(&view), Some(ViewKind::Remote(remote))
                    if remote.agents.iter().any(|agent| agent.pane == pane.0.to_string()))
                {
                    return true;
                }
                let mut command = format!("{action} {}", pane.0);
                if item == AgentMenuItem::OwnerActions {
                    if let Some((workspace, _)) = self.pane_location(view) {
                        let anchor = self.agent_menu.as_ref().map_or((2, 2), |menu| menu.anchor);
                        let (column, row) = self.remote_menu_anchor(workspace, anchor);
                        command.push_str(&format!(" {column} {row}"));
                    }
                }
                (view, command)
            }
            AgentTarget::RemoteSession { view, key } => {
                let action = match item {
                    AgentMenuItem::Resume => "agent_resume",
                    AgentMenuItem::Close => "agent_dismiss",
                    _ => return true,
                };
                if !matches!(self.views.get(&view), Some(ViewKind::Remote(remote))
                    if remote.history.iter().any(|session| session.key == key))
                {
                    return true;
                }
                (view, format!("{action} {}", crate::base64_encode(&key)))
            }
            _ => return false,
        };
        self.send_agent_view_command(view, command);
        true
    }

    pub(crate) fn send_agent_view_command(&mut self, view: PaneId, command: String) {
        let Some((workspace, _)) = self.pane_location(view) else {
            return;
        };
        self.focus_workspace(workspace);
        self.send_workspace_remote(workspace, ClientMessage::Command(command));
    }

    /// Handle only bounded semantic controls received by an existing owner
    /// workspace bridge. Never execute a remote supplied argv or native ID.
    pub(crate) fn handle_remote_agent_command(&mut self, command: &str) -> bool {
        let Some((action, argument)) = command.split_once(' ') else {
            return false;
        };
        match action {
            "agent_resume" | "agent_dismiss" => {
                let index = self
                    .resumable
                    .iter()
                    .position(|session| crate::base64_encode(&history_key(session)) == argument);
                if let Some(index) = index {
                    if action == "agent_resume" {
                        self.resume_session(index);
                    } else {
                        self.dismiss_session(index);
                    }
                }
            }
            "automation_detail" => self.open_automation_detail(argument),
            "agent_rename" | "agent_close" | "agent_pin" | "agent_unpin" => {
                let Some(pane) = argument.parse::<u32>().ok().map(PaneId).filter(|pane| {
                    self.ws().tabs.iter().any(|tab| tab.layout.contains(*pane))
                        && self.panes.contains_key(pane)
                }) else {
                    return true;
                };
                self.focus_pane_global(pane);
                self.open_agent_menu(AgentTarget::Live(pane), 2, 2);
                self.agent_menu_action(match action {
                    "agent_rename" => AgentMenuItem::RenamePane,
                    "agent_close" => AgentMenuItem::Close,
                    "agent_pin" => AgentMenuItem::Pin,
                    _ => AgentMenuItem::Unpin,
                });
            }
            _ => return false,
        }
        true
    }
}
