//! Visible WORKSPACES rows shared by rendering, keyboard navigation and folds.
//! Extends the existing pin/worktree ordering; never changes workspace identity,
//! remote ownership, discovery, or connections.

use super::App;
use crate::config::WorkspaceDisplay;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceSidebarRow {
    Machine(Option<String>),
    Workspace(usize, bool),
}

impl WorkspaceSidebarRow {
    pub(crate) fn height(&self, paths: bool) -> usize {
        match self {
            Self::Machine(_) => 1,
            Self::Workspace(..) => {
                if paths {
                    2
                } else {
                    1
                }
            }
        }
    }
}

/// Row-indexed scrolling with cell-sized rows. A one-line viewport still shows
/// a clipped workspace name, without drawing its path into the next dock.
pub(crate) fn visible_end(
    rows: &[WorkspaceSidebarRow],
    start: usize,
    height: usize,
    paths: bool,
) -> usize {
    let mut used = 0;
    let mut end = start.min(rows.len());
    while end < rows.len() && used < height {
        let next = rows[end].height(paths);
        if used > 0 && used + next > height {
            break;
        }
        used += next;
        end += 1;
    }
    end
}

pub(crate) fn last_scroll(rows: &[WorkspaceSidebarRow], height: usize, paths: bool) -> usize {
    let mut start = rows.len();
    let mut used = 0;
    while start > 0 {
        let next = rows[start - 1].height(paths);
        if used > 0 && used + next > height {
            break;
        }
        used += next;
        start -= 1;
        if used >= height {
            break;
        }
    }
    start
}

impl App {
    /// Background operations temporarily focus panes but do not select a row
    /// for the user. Pair with their existing workspace/tab restoration.
    pub(crate) fn with_preserved_workspace_sidebar<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let collapsed = self.collapsed_workspace_machines.clone();
        let last_shown = self.last_active_ws_shown;
        let result = operation(self);
        self.collapsed_workspace_machines = collapsed;
        self.last_active_ws_shown = last_shown;
        result
    }

    /// Explicit selection also reveals a workspace selected a second time.
    /// Restoration and passive projections still assign their index directly.
    pub(crate) fn focus_workspace(&mut self, workspace: usize) {
        self.active_ws = workspace;
        if self.config.layout.workspace_display == WorkspaceDisplay::Tree
            && self
                .collapsed_workspace_machines
                .contains(&self.workspace_machine(workspace))
        {
            self.reveal_workspace_machine(workspace);
            self.last_active_ws_shown = usize::MAX;
        }
    }

    pub(crate) fn reset_workspace_sidebar_view(&mut self) {
        self.reveal_workspace_machine(self.active_ws);
        self.workspace_cursor = self.workspace_sidebar_position(self.active_ws).unwrap_or(0);
        self.workspaces_scroll = 0;
        self.last_active_ws_shown = usize::MAX;
        self.workspace_machine_rects.clear();
        self.ws_rects.clear();
    }

    pub(crate) fn workspace_machine(&self, workspace: usize) -> Option<String> {
        self.workspaces
            .get(workspace)
            .and_then(|ws| ws.remote.as_ref())
            .map(|remote| remote.host.clone())
    }

    pub(crate) fn workspace_sidebar_position(&self, index: usize) -> Option<usize> {
        self.workspace_sidebar_rows().iter().position(|row| matches!(row, WorkspaceSidebarRow::Workspace(workspace, _) if *workspace == index))
    }

    pub(crate) fn workspace_sidebar_rows(&self) -> Vec<WorkspaceSidebarRow> {
        let order = self.workspace_sidebar_order();
        if self.config.layout.workspace_display == WorkspaceDisplay::Flat {
            return order
                .into_iter()
                .map(|(i, child)| WorkspaceSidebarRow::Workspace(i, child))
                .collect();
        }
        // The shared navigation order is already grouped by machine.
        let mut rows = Vec::with_capacity(self.workspaces.len());
        let mut previous = None;
        for (workspace, child) in order {
            let host = self.workspace_machine(workspace);
            if previous.as_ref() != Some(&host) {
                rows.push(WorkspaceSidebarRow::Machine(host.clone()));
                previous = Some(host.clone());
            }
            if !self.collapsed_workspace_machines.contains(&host) {
                rows.push(WorkspaceSidebarRow::Workspace(workspace, child));
            }
        }
        rows
    }

    pub(crate) fn reveal_workspace_machine(&mut self, workspace: usize) {
        if self.config.layout.workspace_display == WorkspaceDisplay::Tree {
            self.collapsed_workspace_machines
                .remove(&self.workspace_machine(workspace));
        }
    }

    /// Navigation includes folded children so shortcuts can reveal them. Keep
    /// the public pin/worktree index space separate from machine headings.
    pub(crate) fn workspace_sidebar_order(&self) -> Vec<(usize, bool)> {
        let mut order = self.workspace_display_order();
        if self.config.layout.workspace_display == WorkspaceDisplay::Tree {
            order.sort_by(|(a, _), (b, _)| {
                let host = |i: usize| {
                    self.workspaces[i]
                        .remote
                        .as_ref()
                        .map(|remote| remote.host.as_str())
                };
                host(*a).cmp(&host(*b))
            });
        }
        order
    }

    pub(crate) fn toggle_workspace_machine(&mut self, host: Option<String>) {
        if self.config.layout.workspace_display != WorkspaceDisplay::Tree {
            return;
        }
        if !self.collapsed_workspace_machines.remove(&host) {
            self.collapsed_workspace_machines.insert(host.clone());
        }
        self.workspace_cursor = self
            .workspace_sidebar_rows()
            .iter()
            .position(|row| matches!(row, WorkspaceSidebarRow::Machine(owner) if *owner == host))
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::remote::tests::{add_remote_workspace, remote_ui_app};
    use crate::app::{DockKind, Side, SidebarListFocus, Workspace};
    use crate::event::AppEvent;
    use crate::ui::RenderTarget;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    fn key(app: &mut App, code: KeyCode) {
        app.handle_workspaces_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        crate::ui::render_into_without_pane_resize(&mut RenderTarget::new(&mut buffer, area), app);
        buffer
    }

    fn click(app: &mut App, rect: Rect, button: MouseButton) {
        for kind in [MouseEventKind::Down(button), MouseEventKind::Up(button)] {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: rect.x + 2,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
        }
    }

    #[test]
    fn workspace_sidebar_tree_preserves_owner_worktree_and_pin_order() {
        let _env = crate::persist::test_env("workspace-tree-order");
        let mut app = remote_ui_app();
        let (_pane, _input, _) = add_remote_workspace(&mut app);
        app.workspaces[0].worktree = Some(crate::git::WorktreeMembership {
            common_dir: "/repo/.git".into(),
            linked: false,
        });
        app.workspaces[1].worktree = app.workspaces[0].worktree.clone();
        let mut remote = app.workspaces[1].remote.clone().unwrap();
        remote.workspace_id = "linked-remote".into();
        app.workspaces.push(Workspace {
            id: "linked".into(),
            name: "branch".into(),
            cwd: "/repo/linked".into(),
            branch: Some("feature".into()),
            git_ahead_behind: None,
            worktree: Some(crate::git::WorktreeMembership {
                common_dir: "/repo/.git".into(),
                linked: true,
            }),
            tabs: Vec::new(),
            active_tab: 0,
            pinned: true,
            remote: Some(remote),
        });
        assert_eq!(
            app.workspace_sidebar_rows(),
            vec![
                WorkspaceSidebarRow::Workspace(1, false),
                WorkspaceSidebarRow::Workspace(2, true),
                WorkspaceSidebarRow::Workspace(0, false)
            ]
        );
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        let host = Some("dev-207".into());
        assert_eq!(
            app.workspace_sidebar_rows(),
            vec![
                WorkspaceSidebarRow::Machine(None),
                WorkspaceSidebarRow::Workspace(0, false),
                WorkspaceSidebarRow::Machine(host.clone()),
                WorkspaceSidebarRow::Workspace(1, false),
                WorkspaceSidebarRow::Workspace(2, true)
            ]
        );
        app.toggle_workspace_machine(host.clone());
        assert_eq!(app.workspace_sidebar_rows().len(), 3);
        assert_eq!(app.workspace_cursor, 2);
        assert_eq!(app.workspaces[2].remote.as_ref().unwrap().host, "dev-207");
        app.config.layout.workspace_display = WorkspaceDisplay::Flat;
        assert_eq!(
            app.workspace_sidebar_rows().len(),
            3,
            "folds never hide flat rows"
        );
        app.workspaces.clear();
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        assert!(app.workspace_sidebar_rows().is_empty());
        key(&mut app, KeyCode::Enter);
    }

    #[test]
    fn workspace_sidebar_tree_keyboard_targets_visible_rows() {
        let _env = crate::persist::test_env("workspace-tree-keys");
        let mut app = remote_ui_app();
        let (_pane, _input, _) = add_remote_workspace(&mut app);
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        app.active_ws = 0;
        app.focus_workspaces_dock();
        assert_eq!(app.workspace_cursor, 1);
        key(&mut app, KeyCode::Left);
        assert_eq!(app.workspace_cursor, 0);
        key(&mut app, KeyCode::Left);
        assert_eq!(app.workspace_sidebar_rows().len(), 3);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.workspace_sidebar_rows().len(), 2);
        assert_eq!(
            app.active_ws, 0,
            "header activation does not focus another workspace"
        );
        key(&mut app, KeyCode::Right);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.active_ws, 1);
        assert_eq!(app.sidebar_focus, None);
        app.focus_workspaces_dock();
        key(&mut app, KeyCode::Char('a'));
        assert!(app.ws_menu.is_some());
    }

    #[test]
    fn workspace_sidebar_tree_shortcuts_follow_machine_order_and_reveal_folds() {
        let _env = crate::persist::test_env("workspace-tree-shortcuts");
        let mut app = remote_ui_app();
        let (_pane, _input, _) = add_remote_workspace(&mut app);
        app.workspaces[1].pinned = true;
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        app.toggle_workspace_machine(Some("dev-207".into()));
        app.run_cmd(crate::app::Cmd::JumpWorkspace(1));
        assert_eq!(app.active_ws, 0, "Local remains first despite a remote pin");
        app.run_cmd(crate::app::Cmd::JumpWorkspace(2));
        assert_eq!(app.active_ws, 1);
        assert!(app.collapsed_workspace_machines.is_empty());
        app.run_cmd(crate::app::Cmd::NextWorkspace);
        assert_eq!(app.active_ws, 0);
        app.run_cmd(crate::app::Cmd::PrevWorkspace);
        assert_eq!(app.active_ws, 1);
        assert_eq!(
            app.workspace_display_position(1),
            Some(0),
            "public pin order is unchanged"
        );
        app.workspaces.remove(0);
        assert_eq!(
            app.workspace_sidebar_rows(),
            vec![
                WorkspaceSidebarRow::Machine(Some("dev-207".into())),
                WorkspaceSidebarRow::Workspace(0, false)
            ],
            "a direct remote view has no fake Local heading"
        );
    }

    #[test]
    fn workspace_sidebar_tree_reselect_reveals_current_workspace() {
        let _env = crate::persist::test_env("workspace-tree-reselect");
        let mut app = remote_ui_app();
        let (pane, _input, _) = add_remote_workspace(&mut app);
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        app.active_ws = 1;
        let target = app.workspaces[1].remote.clone().unwrap();
        for action in 0..5 {
            draw(&mut app, 180, 48);
            app.toggle_workspace_machine(Some(target.host.clone()));
            draw(&mut app, 180, 48);
            assert!(
                app.workspace_sidebar_position(1).is_none(),
                "ordinary redraw preserves a fold"
            );
            match action {
                0 => {
                    app.dispatch("workspace.focus", &serde_json::json!({"workspace":"1"}))
                        .unwrap();
                }
                1 => app.focus_pane_global(pane),
                2 => app.activate_remote_agent(&target, "42", false),
                3 => app.switcher_activate(super::super::SwitcherTarget::Workspace(1)),
                _ => app.switcher_activate(super::super::SwitcherTarget::Tab { ws: 1, tab: 0 }),
            }
            draw(&mut app, 180, 48);
            assert!(
                app.workspace_sidebar_position(1).is_some(),
                "selection {action} must reveal the same active workspace"
            );
            assert_eq!(app.active_ws, 1);
        }
    }

    #[test]
    fn workspace_sidebar_tree_reindex_preserves_active_machine_fold() {
        let _env = crate::persist::test_env("workspace-tree-reindex");
        let mut app = remote_ui_app();
        let (_pane, _input, _) = add_remote_workspace(&mut app);
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        app.active_ws = 1;
        draw(&mut app, 180, 48);
        app.toggle_workspace_machine(Some("dev-207".into()));
        draw(&mut app, 180, 48);
        app.close_workspace(0);
        assert_eq!(app.active_ws, 0);
        draw(&mut app, 180, 48);
        assert!(
            app.workspace_sidebar_position(0).is_none(),
            "same identity is not a selection even when its index changes"
        );
    }

    #[test]
    fn workspace_sidebar_tree_reveals_replacement_at_same_index() {
        let _env = crate::persist::test_env("workspace-tree-replacement");
        let mut app = remote_ui_app();
        let (_pane, _input, _) = add_remote_workspace(&mut app);
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        app.active_ws = 0;
        draw(&mut app, 180, 48);
        app.toggle_workspace_machine(Some("dev-207".into()));
        app.close_workspace(0);
        assert_eq!(app.active_ws, 0);
        draw(&mut app, 180, 48);
        assert!(app.workspace_sidebar_position(0).is_some());
    }

    #[test]
    fn workspace_sidebar_tree_scroll_uses_actual_row_heights() {
        let rows = vec![
            WorkspaceSidebarRow::Machine(None),
            WorkspaceSidebarRow::Workspace(0, false),
            WorkspaceSidebarRow::Machine(Some("host".into())),
            WorkspaceSidebarRow::Workspace(1, false),
        ];
        assert_eq!(visible_end(&rows, 0, 3, true), 2);
        assert_eq!(visible_end(&rows, 0, 2, true), 1);
        assert_eq!(visible_end(&rows, 1, 1, true), 2);
        assert_eq!(last_scroll(&rows, 3, true), 2);
        assert_eq!(last_scroll(&rows, 3, false), 1);
        assert_eq!(visible_end(&rows, 0, 0, true), 0);
        assert_eq!(last_scroll(&[], 3, true), 0);
    }

    #[test]
    fn workspace_sidebar_tree_mouse_docks_tags_and_projection_isolation() {
        let _env = crate::persist::test_env("workspace-tree-mouse");
        for side in [Side::Left, Side::Right] {
            for paths in [true, false] {
                let mut app = remote_ui_app();
                let (_pane, receiver, _) = add_remote_workspace(&mut app);
                app.workspaces[0].name = "local-project".into();
                app.workspaces[1].name = "remote-project".into();
                app.config.layout.workspace_display = WorkspaceDisplay::Tree;
                app.config.layout.workspace_paths = paths;
                app.active_ws = 0;
                app.move_dock(&DockKind::Workspaces, side);
                app.sidebars.get_mut(side).visible = true;
                let buffer = draw(&mut app, 180, 48);
                let remote_row = app.ws_rects.iter().find(|(i, _)| *i == 1).unwrap().1;
                let text: String = (remote_row.x..remote_row.right())
                    .map(|x| buffer[(x, remote_row.y)].symbol())
                    .collect();
                assert!(text.contains("remote-project"));
                assert!(
                    !text.contains("dev-207"),
                    "tree children must not repeat hostname tags"
                );
                assert_eq!(remote_row.height, if paths { 2 } else { 1 });
                let host = app
                    .workspace_machine_rects
                    .iter()
                    .find(|(host, _)| host.is_some())
                    .unwrap()
                    .1;
                click(&mut app, host, MouseButton::Left);
                draw(&mut app, 180, 48);
                assert_eq!(app.ws_rects.len(), 1);
                assert_eq!(app.active_ws, 0);
                assert!(
                    receiver.try_recv().is_err(),
                    "fold press leaked to remote owner"
                );
                let host = app
                    .workspace_machine_rects
                    .iter()
                    .find(|(host, _)| host.is_some())
                    .unwrap()
                    .1;
                click(&mut app, host, MouseButton::Left);
                draw(&mut app, 180, 48);
                let remote_row = app.ws_rects.iter().find(|(i, _)| *i == 1).unwrap().1;
                click(&mut app, remote_row, MouseButton::Left);
                assert_eq!(app.active_ws, 1);
                draw(&mut app, 180, 48);
                let remote_row = app.ws_rects.iter().find(|(i, _)| *i == 1).unwrap().1;
                click(&mut app, remote_row, MouseButton::Right);
                assert!(app.ws_menu.is_some());
                draw(&mut app, 180, 48);
                let content = app.pane_content_rects[0].1;
                click(
                    &mut app,
                    Rect::new(content.right() - 5, content.y + content.height / 2, 1, 1),
                    MouseButton::Left,
                );
                assert!(app.ws_menu.is_none());
                assert!(
                    receiver.try_recv().is_err(),
                    "menu dismissal leaked to owner"
                );

                // An externally selected workspace is revealed by the active
                // render, but a passive client's tiny frame cannot unfold it.
                app.toggle_workspace_machine(Some("dev-207".into()));
                let hits = app.workspace_machine_rects.clone();
                let folds = app.collapsed_workspace_machines.clone();
                app.last_active_ws_shown = usize::MAX;
                app.workspace_cursor = 100;
                let area = Rect::new(0, 0, 90, 12);
                let mut passive = Buffer::empty(area);
                crate::ui::render_projection(&mut RenderTarget::new(&mut passive, area), &mut app);
                assert_eq!(app.workspace_machine_rects, hits);
                assert_eq!(app.collapsed_workspace_machines, folds);
                assert_eq!(app.workspace_cursor, 100);
                draw(&mut app, 180, 48);
                assert!(!app
                    .collapsed_workspace_machines
                    .contains(&Some("dev-207".into())));

                app.config.layout.workspace_display = WorkspaceDisplay::Flat;
                app.reset_workspace_sidebar_view();
                let buffer = draw(&mut app, 180, 48);
                assert!(app.workspace_machine_rects.is_empty());
                let row = app.ws_rects.iter().find(|(i, _)| *i == 1).unwrap().1;
                let text: String = (row.x..row.right())
                    .map(|x| buffer[(x, row.y)].symbol())
                    .collect();
                assert!(
                    text.contains("[dev-207]"),
                    "flat mode retains ownership tag: {text}"
                );
                app.unmount_dock(&DockKind::Workspaces);
                draw(&mut app, 180, 48);
                assert!(app.ws_rects.is_empty() && app.workspace_machine_rects.is_empty());
                assert!(
                    app.panes.is_empty(),
                    "this regression owns no PTYs or SSH process"
                );
            }
        }
    }

    #[test]
    fn workspace_sidebar_tree_clears_hits_in_tiny_or_empty_viewports() {
        let _env = crate::persist::test_env("workspace-tree-small");
        let mut app = remote_ui_app();
        app.config.layout.workspace_display = WorkspaceDisplay::Tree;
        draw(&mut app, 120, 40);
        assert!(!app.workspace_machine_rects.is_empty());
        draw(&mut app, 20, 4);
        assert!(app.workspace_machine_rects.is_empty() && app.ws_rects.is_empty());
        assert_eq!(app.workspaces_area, Rect::ZERO);
        draw(&mut app, 120, 40);
        assert!(!app.workspace_machine_rects.is_empty());
        app.workspaces.clear();
        draw(&mut app, 120, 40);
        assert!(app.workspace_machine_rects.is_empty() && app.ws_rects.is_empty());
        assert_eq!(app.workspaces_area, Rect::ZERO);
    }

    #[test]
    fn workspace_sidebar_config_api_validates_without_changing_identity() {
        let _env = crate::persist::test_env("workspace-tree-api");
        let mut app = remote_ui_app();
        let (_pane, receiver, _) = add_remote_workspace(&mut app);
        app.config.remote_hosts = vec!["dev-207".into()];
        let before = app
            .dispatch("workspace.list", &serde_json::json!({}))
            .unwrap();
        for display in ["tree", "flat"] {
            let result = app
                .dispatch(
                    "config.patch",
                    &serde_json::json!({"patch":{"layout":{"workspace_display":display}}}),
                )
                .unwrap();
            assert_eq!(result["config"]["layout"]["workspace_display"], display);
            assert_eq!(
                before,
                app.dispatch("workspace.list", &serde_json::json!({}))
                    .unwrap()
            );
        }
        for invalid in [
            serde_json::json!("tre"),
            serde_json::json!(1),
            serde_json::Value::Null,
        ] {
            let result = app.dispatch(
                "config.patch",
                &serde_json::json!({"patch":{"layout":{"workspace_display":invalid}}}),
            );
            assert_eq!(result.unwrap_err().0, "invalid_request");
            assert_eq!(app.config.layout.workspace_display, WorkspaceDisplay::Flat);
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(app.sidebar_focus, None::<SidebarListFocus>);
    }
}
