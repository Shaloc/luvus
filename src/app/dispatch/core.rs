//! Core JSON API handlers.

use super::params::*;
use super::*;

impl App {
    pub(super) fn api_ping(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        Ok(json!({
            "type":"pong",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol":1,
            "transport_protocol": crate::ipc::protocol::PROTOCOL_VERSION,
            "client_protocol":crate::ipc::protocol::PROTOCOL_VERSION,
            "session": crate::session::display_name()
        }))
    }

    pub(super) fn api_uhp_capabilities(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &[])?;
            let mut capabilities =
                crate::api::capabilities(crate::ipc::api::current_sequence(&self.events));
            if let Some(object) = capabilities.as_object_mut() {
                object.insert("session".into(), json!(crate::session::display_name()));
                object.insert(
                    "server_generation".into(),
                    json!(self.backend_server_generation),
                );
            }
            Ok(capabilities)
        }
    }

    pub(super) fn api_config_get(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &[])?;
            Ok(json!({"type":"config", "config":self.config}))
        }
    }

    pub(super) fn api_config_patch(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &["patch"])?;
            let patch = p.get("patch").ok_or_else(|| {
                (
                    "invalid_request".to_string(),
                    "config.patch needs a patch object".to_string(),
                )
            })?;
            let next = patched_config(&self.config, patch)?;
            self.apply_socket_config(next, Some(patch))?;
            Ok(json!({"type":"config", "config":self.config}))
        }
    }

    pub(super) fn api_server_reload_config(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &[])?;
            let next = crate::config::load();
            self.apply_socket_config(next, None)?;
            Ok(json!({"type":"config_reloaded", "config":self.config}))
        }
    }

    pub(super) fn api_server_agent_manifests(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &[])?;
            Ok(json!({
                "type":"agent_manifests",
                "rules":self.manifests.rule_count(),
                "agents":self.manifests.agent_names(),
            }))
        }
    }

    pub(super) fn api_server_reload_agent_manifests(
        &mut self,
        method: &str,
        p: &Value,
    ) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &[])?;
            let manifests = crate::detect::Manifests::load(&crate::persist::ensure_manifests_dir());
            self.apply_socket_manifests(manifests);
            let rules = self.manifests.rule_count();
            Ok(json!({"type":"agent_manifests_reloaded","rules":rules}))
        }
    }

    pub(super) fn api_session_snapshot(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            reject_api_fields(p, &[])?;
            Ok(self.runtime_snapshot())
        }
    }

    pub(super) fn api_server_stop(&mut self, method: &str, p: &Value) -> DispatchResult {
        let _ = (method, p);
        {
            self.should_quit = true;
            Ok(json!({"type":"ok"}))
        }
    }
}

impl App {
    /// One coherent, sequence-fenced model for orchestrators. Unlike the
    /// presentation-oriented list methods this spans every workspace and tab,
    /// includes non-terminal views explicitly, and never reads terminal text.
    pub(crate) fn runtime_snapshot(&self) -> Value {
        // Invert once per snapshot rather than scanning every alias per pane.
        // Backend titles intentionally do not override operator-assigned names.
        let agent_names: HashMap<_, _> = self
            .agent_names
            .iter()
            .map(|(name, pane)| (*pane, name))
            .collect();
        let mut workspaces = Vec::with_capacity(self.workspaces.len());
        for (workspace_index, workspace) in self.workspaces.iter().enumerate() {
            let mut tabs = Vec::with_capacity(workspace.tabs.len());
            for (tab_index, tab) in workspace.tabs.iter().enumerate() {
                let kind = if tab.is_git() {
                    "git"
                } else if tab.is_orch() {
                    "orchestration"
                } else if tab.is_mission() {
                    "mission_control"
                } else {
                    "panes"
                };
                let panes: Vec<Value> = tab
                    .layout
                    .leaves()
                    .into_iter()
                    .map(|pane_id| {
                        if let Some(pane) = self.panes.get(&pane_id) {
                            let runtime = pane.terminal_runtime();
                            let status = self.status.get(&pane_id);
                            json!({
                                "pane_id":pane_id.0.to_string(),
                                "kind":"terminal",
                                "focused":workspace_index == self.active_ws
                                    && tab_index == workspace.active_tab
                                    && tab.layout.focus == pane_id,
                                "cwd":pane.cwd.display().to_string(),
                                "terminal_id":runtime.as_ref().map(|runtime| runtime.terminal_id.clone()),
                                "root_process":runtime.as_ref().map(|runtime| json!({
                                    "pid":runtime.pid,
                                    "start_marker":runtime.start_marker,
                                })),
                                "content_revision":pane.content_revision(),
                                "agent_name":agent_names.get(&pane_id).copied(),
                                "agent":status.map(|status| status.agent.clone()),
                                "is_agent":status.is_some_and(|status| self.manifests.is_agent(&status.agent) || status.agent_session.is_some() || status.agent_report.is_some()),
                                "title":self.cached_pane_title(pane_id),
                                "agent_pinned":self.pinned_agents.contains(&pane_id),
                                "workspace_focused":tab_index == workspace.active_tab && tab.layout.focus == pane_id,
                                "agent_status":status.map(|status| state_str(status.state)),
                                "agent_authority":status.map(|status| status.identity_source),
                                "agent_session":status.and_then(|status| status.agent_session.as_ref().map(|session| session.session_id.clone())),
                            })
                        } else {
                            json!({
                                "pane_id":pane_id.0.to_string(),
                                "kind":"view",
                                "workspace_focused":tab_index == workspace.active_tab && tab.layout.focus == pane_id,
                                "focused":workspace_index == self.active_ws
                                    && tab_index == workspace.active_tab
                                    && tab.layout.focus == pane_id,
                            })
                        }
                    })
                    .collect();
                tabs.push(json!({
                    "index":tab_index + 1,
                    "name":tab.name,
                    "kind":kind,
                    "active":tab_index == workspace.active_tab,
                    "panes":panes,
                }));
            }
            workspaces.push(json!({
                "id":workspace.id,
                "index":workspace_index + 1,
                "name":workspace.name,
                "cwd":workspace.cwd.display().to_string(),
                "branch":workspace.branch,
                "worktree":workspace.worktree.as_ref().map(|membership| json!({"common_dir":membership.common_dir,"linked":membership.linked})),
                "host":workspace.remote.as_ref().map(|remote| remote.host.clone()),
                "pinned":workspace.pinned,
                "active":workspace_index == self.active_ws,
                "tabs":tabs,
                "agent_history":self.agent_history_rows(workspace_index),
                "scheduled_agents":self.scheduled_agent_rows().into_iter().filter(|row| row.workspace_id == workspace.id).collect::<Vec<_>>(),
            }));
        }
        json!({
            "type":"session_snapshot",
            "protocol":{
                "name":crate::api::PROTOCOL_NAME,
                "major":crate::api::PROTOCOL_MAJOR,
                "minor":crate::api::PROTOCOL_MINOR,
            },
            "session":crate::session::display_name(),
            "server_generation":self.backend_server_generation,
            "event_sequence":crate::ipc::api::current_sequence(&self.events),
            "remote_display":crate::ipc::protocol::remote_display_capabilities(),
            "workspaces":workspaces,
        })
    }

    pub(in crate::app) fn apply_socket_config(
        &mut self,
        next: crate::config::Config,
        persist_patch: Option<&Value>,
    ) -> Result<(), (String, String)> {
        let prefix = keys::PrefixSpec::parse(&next.prefix).ok_or_else(|| {
            (
                "invalid_request".to_string(),
                "config prefix must be F1-F12 or a valid Ctrl/Alt chord".to_string(),
            )
        })?;
        keys::validate_direct_keybindings(&next.direct_keybindings)
            .map_err(|message| ("invalid_request".to_string(), message))?;
        for host in &next.remote_hosts {
            crate::session::remote::validate_host_alias(host)
                .map_err(|message| ("invalid_request".to_string(), message))?;
        }
        let provider_changed = next.worktree != self.config.worktree;
        crate::worktree::validate_config(&next.worktree, provider_changed.then_some(&self.modules))
            .map_err(|message| ("invalid_request".to_string(), message))?;
        if self.theme_registry.get(&next.theme).is_none() && next.theme != "terminal" {
            return Err((
                "invalid_request".to_string(),
                format!("theme `{}` is not installed", next.theme),
            ));
        }
        if persist_patch
            .and_then(Value::as_object)
            .is_some_and(|patch| {
                !patch.is_empty()
                    && patch
                        .keys()
                        .all(|key| matches!(key.as_str(), "theme" | "language"))
            })
        {
            // A remote preference change must not reset scroll/layout, rediscover
            // this owner's SSH hosts, or propagate back to the sender.
            let patch = persist_patch.expect("preference patch");
            if patch.get("theme").is_some() {
                self.apply_theme_locally(&next.theme);
            }
            if patch.get("language").is_some() {
                self.apply_language_locally(&next.language);
            }
            self.emit_event("config.changed", json!({}));
            return Ok(());
        }
        let theme = self.theme_registry.theme_or_default(&next.theme);
        let sidebars = Sidebars::from_config(&next.sidebars());
        let keymap = keys::build_keymap(&next.keybindings);
        let direct_keymap = keys::build_direct_keymap(&next.direct_keybindings);
        let history_budget = next.scrollback_bytes();
        self.set_effective_theme(&next.theme, theme);
        self.catalog = crate::i18n::by_code(&next.language);
        self.prefix = prefix;
        self.keymap = keymap;
        self.direct_keymap = direct_keymap;
        self.sidebars = sidebars;
        self.file_tree.show_hidden = next.layout.files_show_hidden;
        self.file_tree.scroll = 0;
        self.apply_agents_filter(next.agents_active_only);
        self.apply_agents_scope(next.agents_this_workspace);
        crate::layout::set_gaps(next.layout.col_gap, next.layout.row_gap);
        for pane in self.panes.values() {
            pane.set_history_budget(history_budget);
        }
        let workspace_display_changed =
            self.config.layout.workspace_display != next.layout.workspace_display;
        self.config = next;
        if workspace_display_changed {
            self.reset_workspace_sidebar_view();
        }
        self.changelog_rows = None;
        if let Some(patch) = persist_patch {
            self.persist_config_patch(patch);
        } else {
            self.reset_config_baseline();
        }
        self.emit_event("config.changed", json!({}));
        self.start_merged_remote_sessions();
        Ok(())
    }

    pub(in crate::app) fn apply_socket_manifests(&mut self, manifests: crate::detect::Manifests) {
        self.manifests = manifests;
        for status in self.status.values_mut() {
            status.force_detect = true;
        }
        self.emit_event(
            "server.agent_manifests_reloaded",
            json!({"rules":self.manifests.rule_count()}),
        );
    }
}
