use super::*;

pub struct BrowserPrompt {
    /// Stable owner identity, not the workspace that happens to be focused later.
    pub workspace_id: String,
    pub buffer: String,
    pub cursor: usize,
    pub invalid: bool,
}

impl App {
    pub(crate) fn open_browser_prompt(&mut self, workspace: usize) {
        let Some(ws) = self
            .workspaces
            .get(workspace)
            .filter(|ws| ws.remote.is_none())
        else {
            return;
        };
        self.browser_prompt = Some(BrowserPrompt {
            workspace_id: ws.id.clone(),
            buffer: String::new(),
            cursor: 0,
            invalid: false,
        });
    }

    pub(crate) fn handle_browser_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.browser_prompt = None,
            KeyCode::Enter => {
                let Some(mut prompt) = self.browser_prompt.take() else {
                    return;
                };
                let url = prompt.buffer.trim();
                if !url.is_empty() && !crate::browser::valid_url(url) {
                    prompt.invalid = true;
                    self.browser_prompt = Some(prompt);
                    return;
                }
                let Some(workspace) = self
                    .workspaces
                    .iter()
                    .position(|ws| ws.id == prompt.workspace_id && ws.remote.is_none())
                else {
                    self.show_toast(self.catalog.tab_changed_reopen_menu);
                    return;
                };
                let url = if url.is_empty() {
                    crate::browser::BLANK_URL
                } else {
                    url
                };
                self.insert_browser_view(workspace, None, url.into());
            }
            _ => {
                if let Some(prompt) = self.browser_prompt.as_mut() {
                    if line_edit::edit_line(
                        &mut prompt.buffer,
                        &mut prompt.cursor,
                        key,
                        8192,
                        |text| text.len() <= 8192 && !text.chars().any(char::is_control),
                    ) {
                        prompt.invalid = false;
                    }
                }
            }
        }
    }

    /// Shared native-view insertion for the CLI split and the context-menu tab.
    /// Only resolved owner workspaces reach this method; no PTY is created.
    pub(crate) fn insert_browser_view(
        &mut self,
        workspace: usize,
        tab: Option<usize>,
        url: String,
    ) -> PaneId {
        let id = PaneId::alloc();
        self.views.insert(id, ViewKind::Browser(url));
        let ws = &mut self.workspaces[workspace];
        if let Some(tab) = tab {
            let layout = &mut ws.tabs[tab].layout;
            layout.split_focused(Axis::Col, id);
            layout.focus = id;
        } else {
            let mut tab = Tab::panes(TileLayout::new(id));
            tab.name = Some(self.catalog.browser_open.into());
            ws.tabs.push(tab);
            ws.active_tab = ws.tabs.len() - 1;
            let position = ws.active_tab + 1;
            self.emit_event("tab.created", json!({"tab":position.to_string()}));
        }
        self.session_dirty = true;
        self.emit_event("pane.created", json!({"pane":id.0.to_string()}));
        id
    }

    pub(crate) fn activate_browser_panel(&mut self, pane: PaneId) {
        let Some(ViewKind::Browser(url)) = self.views.get(&pane) else {
            return;
        };
        let (client, origin) = self.input_source();
        self.pending_browser = Some((
            pane,
            client,
            crate::browser::LaunchRequest::owner(crate::browser::Request {
                proxied: true,
                url: url.clone(),
                hosts: Vec::new(),
                origin,
            }),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_launch_captures_owner_proxy_environment() {
        let _env = persist::test_env("browser-owner-proxy");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let pane = app.insert_browser_view(0, None, "about:blank".into());
        let previous = std::env::var_os("http_proxy");
        std::env::set_var("http_proxy", "http://owner-proxy.invalid:8123");
        app.activate_browser_panel(pane);
        match previous {
            Some(value) => std::env::set_var("http_proxy", value),
            None => std::env::remove_var("http_proxy"),
        }
        let (_, _, launch) = app.pending_browser.take().unwrap();
        let encoded = serde_json::to_value(launch).unwrap();
        assert_eq!(encoded["proxy"]["http"], "http://owner-proxy.invalid:8123");
    }

    #[test]
    fn browser_menu_creates_blank_tab_without_pty_and_restores_it() {
        let _env = persist::test_env("browser-menu-blank");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let original = app.layout().focus;
        let ptys = app.panes.len();
        app.open_tab_menu(0, 1, 1);
        assert!(app.tab_menu_items().contains(&TabMenuItem::NewBrowserTab));
        app.tab_menu_action(TabMenuItem::NewBrowserTab);
        assert!(app.browser_prompt.as_ref().unwrap().buffer.is_empty());
        assert!(!crate::ui::chrome_uncovered(&app));
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(app.browser_prompt.is_none());
        assert_eq!(app.ws().tabs.len(), 2);
        assert_eq!(app.ws().active_tab, 1);
        let browser = app.layout().focus;
        assert!(
            matches!(app.views.get(&browser), Some(ViewKind::Browser(url)) if url == crate::browser::BLANK_URL)
        );
        assert_eq!(app.panes.len(), ptys);
        assert_eq!(app.ws().tabs[0].layout.leaves(), [original]);
        assert!(app.pending_browser.is_none());
        let snapshot = persist::snapshot(&app);
        drop(app);
        let (tx, _rx) = std::sync::mpsc::channel();
        let restored = App::from_snapshot(snapshot, tx).unwrap();
        assert_eq!(restored.ws().tabs.len(), 2);
        assert!(
            matches!(restored.views.get(&restored.layout().focus), Some(ViewKind::Browser(url)) if url == crate::browser::BLANK_URL)
        );
        assert_eq!(restored.panes.len(), ptys);
        assert!(restored.pending_browser.is_none());
    }

    #[test]
    fn browser_prompt_validates_cancels_and_keeps_its_workspace_target() {
        let _env = persist::test_env("browser-menu-target");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let pane = app.layout().focus;
        app.open_pane_menu(pane, 20, 5);
        assert!(app.pane_menu_items().contains(&PaneMenuItem::NewBrowserTab));
        app.pane_menu_action(PaneMenuItem::NewBrowserTab);
        for c in "file:///etc/passwd".chars() {
            app.handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(app.browser_prompt.as_ref().unwrap().invalid);
        assert_eq!(app.ws().tabs.len(), 1);
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(app.browser_prompt.is_none());
        assert_eq!(app.ws().tabs.len(), 1);
        app.open_browser_prompt(0);
        let other = persist::config_dir().join("other");
        std::fs::create_dir_all(&other).unwrap();
        app.dispatch("workspace.open", &json!({"path":other}))
            .unwrap();
        let focus = app.layout().focus;
        let prompt = app.browser_prompt.as_mut().unwrap();
        prompt.buffer = "https://example.test/?x=1&y=2".into();
        prompt.cursor = prompt.buffer.len();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.active_ws, 1);
        assert_eq!(app.layout().focus, focus);
        assert_eq!(app.workspaces[0].tabs.len(), 2);
        assert_eq!(app.ws().tabs.len(), 1);
        let id = app.workspaces[0].tabs[1].layout.focus;
        assert!(
            matches!(app.views.get(&id), Some(ViewKind::Browser(url)) if url == "https://example.test/?x=1&y=2")
        );
        // A closed target must not create a tab in the remaining workspace.
        app.open_browser_prompt(0);
        app.dispatch("workspace.close", &json!({"workspace":0}))
            .unwrap();
        let before = app.views.len();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.views.len(), before);
        assert_eq!(app.ws().tabs.len(), 1);
    }

    #[test]
    fn browser_panel_rejects_dashboard_placeholders_before_mutation() {
        let _env = persist::test_env("browser-panel-dashboard");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let foreground = app.layout().focus;
        for kind in ["git", "orch", "mission"] {
            let placeholder = PaneId::alloc();
            let mut dashboard = Tab::panes(TileLayout::new(placeholder));
            match kind {
                "git" => {
                    dashboard.git = Some(Box::new(crate::git::GitView::new(app.ws().cwd.clone())))
                }
                "orch" => dashboard.orch = true,
                "mission" => dashboard.mission = true,
                _ => unreachable!(),
            }
            app.workspaces[0].tabs.push(dashboard);
            app.session_dirty = false;
            let revision = crate::ipc::api::current_sequence(&app.events);
            let views = app.views.len();
            assert!(
                app.dispatch(
                    "browser.panel",
                    &json!({"pane":placeholder.0,"url":"http://localhost:3000"})
                )
                .is_err(),
                "{kind} placeholder is not a live terminal or native pane"
            );
            assert_eq!(app.views.len(), views);
            assert_eq!(app.workspaces[0].tabs[1].layout.leaves(), [placeholder]);
            assert_eq!(app.layout().focus, foreground);
            assert_eq!(crate::ipc::api::current_sequence(&app.events), revision);
            assert!(!app.session_dirty);
            app.workspaces[0].tabs.pop();
        }
    }

    #[test]
    fn browser_panel_targets_explicit_workspace_and_preserves_pty() {
        let _env = persist::test_env("browser-panel-owner");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        let anchor = app.layout().focus;
        let before = app.panes.len();
        let url = "http://localhost:3000/test?x=1&y=2";
        for params in [
            json!({"url":url}),
            json!({"pane":anchor.0,"url":url,"host":"bad"}),
            json!({"pane":anchor.0,"url":"file:///etc/passwd"}),
        ] {
            assert!(app.dispatch("browser.panel", &params).is_err());
            assert_eq!(app.layout().len(), 1);
        }
        let other = persist::config_dir().join("other");
        std::fs::create_dir_all(&other).unwrap();
        app.dispatch("workspace.open", &json!({"path":other}))
            .unwrap();
        assert_eq!(app.active_ws, 1);
        let foreground = app.layout().focus;
        let before = before + 1;
        let revision = crate::ipc::api::current_sequence(&app.events);
        let result = app
            .dispatch("browser.panel", &json!({"pane":anchor.0,"url":url}))
            .unwrap();
        let id = PaneId(result["pane"].as_str().unwrap().parse().unwrap());
        let events = crate::ipc::api::replayed_events_after(&app.events, revision);
        let expected_pane = id.0.to_string();
        assert!(
            events.iter().any(|event| event["event"] == "pane.created"
                && event["data"]["pane"] == expected_pane.as_str()),
            "background native pane creation must advance the revision and announce topology: {events:?}"
        );
        assert!(matches!(app.views.get(&id), Some(ViewKind::Browser(value)) if value == url));
        assert_eq!(app.panes.len(), before);
        assert!(app.panes.contains_key(&anchor));
        assert_eq!(app.workspaces[0].tabs[0].layout.len(), 2);
        assert_eq!(app.active_ws, 1);
        assert_eq!(app.layout().focus, foreground);
        assert_eq!(app.layout().len(), 1);
        assert!(
            app.pending_browser.is_none(),
            "creation must not launch a browser"
        );
        app.with_input_source(Some(3), Some(17), |app| app.activate_browser_panel(id));
        let (target, client, request) = app.pending_browser.take().unwrap();
        assert_eq!(target, id);
        assert_eq!(client, Some(3));
        assert_eq!(request.request.origin, Some(17));
        assert!(request.request.hosts.is_empty());
        let snapshot = persist::snapshot(&app);
        drop(app);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut restored = App::from_snapshot(snapshot, tx).unwrap();
        let browser = *restored
            .views
            .iter()
            .find_map(|(id, view)| {
                matches!(view, ViewKind::Browser(value) if value == url).then_some(id)
            })
            .unwrap();
        assert_eq!(restored.panes.len(), before);
        assert!(
            restored.pending_browser.is_none(),
            "restore never launches Chrome"
        );
        restored
            .dispatch("pane.close", &json!({"pane":browser.0}))
            .unwrap();
        assert!(!restored.views.contains_key(&browser));
        assert_eq!(restored.panes.len(), before);
    }
}
