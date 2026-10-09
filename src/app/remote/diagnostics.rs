//! Read-only host diagnostics from existing watcher, display and workspace evidence.
//! No SSH probes, retries or additional transport ownership are introduced here.
use super::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostHealth {
    Connected,
    Partial,
    Connecting,
    Disconnected,
}
impl HostHealth {
    pub(crate) fn label(self, cat: &crate::i18n::Catalog) -> &'static str {
        cat.host_health[self as usize]
    }
    fn merge(self, other: Self) -> Self {
        if self == other {
            self
        } else if self == Self::Connecting && other == Self::Disconnected
            || self == Self::Disconnected && other == Self::Connecting
        {
            Self::Connecting
        } else {
            Self::Partial
        }
    }
}

pub(crate) struct HostInspect {
    pub host: String,
    pub anchor: (u16, u16),
    pub selected: usize,
    pub rects: Vec<Rect>,
    pub details: Option<String>,
    pub errors_only: bool,
    pub scroll: usize,
    pub max_scroll: usize,
    pub modal: Option<Rect>,
}

// Control characters must not become terminal commands or disturb the report.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(4096)
        .collect()
}

impl App {
    /// Pool readiness is authoritative for a pooled session. An inactive
    /// workspace's last frame/error is cached evidence, not a separate socket.
    pub(crate) fn remote_host_health(&self) -> HashMap<&str, HostHealth> {
        let mut states = HashMap::new();
        let mut add = |host, state| {
            states
                .entry(host)
                .and_modify(|s: &mut HostHealth| *s = s.merge(state))
                .or_insert(state);
        };
        for link in self.remote_session_displays.values() {
            add(
                link.target.host.as_str(),
                if link.input.is_some() {
                    HostHealth::Connected
                } else {
                    HostHealth::Connecting
                },
            );
        }
        for watcher in self.remote_session_watchers.values() {
            add(
                watcher.target.host.as_str(),
                if watcher.retry_pending || watcher.refresh_projections {
                    HostHealth::Connecting
                } else {
                    HostHealth::Connected
                },
            );
        }
        for view in self.views.values() {
            let ViewKind::Remote(view) = view else {
                continue;
            };
            let pooled = self.remote_session_displays.values().any(|link| {
                link.target.host == view.target.host && link.target.session == view.target.session
            });
            if pooled {
                continue;
            }
            add(
                view.target.host.as_str(),
                match view.state {
                    RemoteViewState::Ready => HostHealth::Connected,
                    RemoteViewState::Connecting => HostHealth::Connecting,
                    RemoteViewState::Disconnected => HostHealth::Disconnected,
                },
            );
        }
        // Discovery errors are historical once a live transport has stronger evidence.
        for status in &self.remote_host_status {
            states
                .entry(status.host.as_str())
                .or_insert(if status.error.is_some() {
                    HostHealth::Disconnected
                } else {
                    HostHealth::Connecting
                });
        }
        states
    }

    pub(crate) fn host_diagnostics(&self, host: &str, errors_only: bool) -> String {
        use std::fmt::Write;
        let cat = self.catalog;
        let state = self
            .remote_host_health()
            .get(host)
            .copied()
            .unwrap_or(HostHealth::Connecting);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut out = format!(
            "{} · {}\n{}: {now}\n",
            clean(host),
            state.label(cat),
            cat.host_snapshot
        );
        let mut errors = 0;
        if !errors_only {
            let mut watchers: Vec<_> = self
                .remote_session_watchers
                .values()
                .filter(|w| w.target.host == host)
                .collect();
            watchers.sort_by_key(|w| &w.target.session);
            for watcher in watchers {
                let state = if watcher.retry_pending || watcher.refresh_projections {
                    HostHealth::Connecting
                } else {
                    HostHealth::Connected
                };
                let _ = writeln!(
                    out,
                    "{} · {}: {} (generation={})",
                    cat.host_channels[0],
                    clean(&watcher.target.session),
                    state.label(cat),
                    watcher.generation
                );
            }
            let mut links: Vec<_> = self
                .remote_session_displays
                .values()
                .filter(|l| l.target.host == host)
                .collect();
            links.sort_by_key(|l| &l.target.session);
            for link in links {
                let state = if link.input.is_some() {
                    HostHealth::Connected
                } else {
                    HostHealth::Connecting
                };
                let _ = writeln!(
                    out,
                    "{} · {}: {} (generation={}, epoch={})",
                    cat.host_channels[1],
                    clean(&link.target.session),
                    state.label(cat),
                    link.generation,
                    link.epoch
                );
            }
        }
        for (index, ws) in self.workspaces.iter().enumerate() {
            let Some(view) = self
                .remote_workspace_view(index)
                .filter(|v| v.target.host == host)
            else {
                continue;
            };
            let pooled = self.remote_session_displays.values().any(|link| {
                link.target.host == view.target.host
                    && link.target.session == view.target.session
                    && link.input.is_some()
            });
            let cached = pooled && !view.projection.active;
            let state = match view.state {
                RemoteViewState::Ready => HostHealth::Connected,
                RemoteViewState::Connecting => HostHealth::Connecting,
                RemoteViewState::Disconnected => HostHealth::Disconnected,
            };
            if !errors_only || view.error.is_some() {
                let _ = writeln!(
                    out,
                    "\n{} · {} / {}: {}",
                    cat.host_channels[2],
                    clean(&view.target.session),
                    clean(&ws.name),
                    if cached {
                        cat.host_standby
                    } else {
                        state.label(cat)
                    }
                );
                if !errors_only {
                    let _ = writeln!(
                        out,
                        "  workspace_id={} · generation={} · epoch={}",
                        clean(&view.target.workspace_id),
                        view.generation,
                        view.projection.epoch
                    );
                }
                if let Some(error) = &view.error {
                    errors += 1;
                    let _ = writeln!(
                        out,
                        "  {}{}: {}",
                        cat.host_errors,
                        if cached {
                            format!(" ({})", cat.host_cached)
                        } else {
                            String::new()
                        },
                        clean(error)
                    );
                }
            }
        }
        for status in self.remote_host_status.iter().filter(|s| s.host == host) {
            if let Some(error) = &status.error {
                errors += 1;
                let _ = writeln!(
                    out,
                    "\n{} ({}): {}",
                    cat.host_channels[3],
                    cat.host_cached,
                    clean(error)
                );
            }
        }
        if errors_only && errors == 0 {
            let _ = writeln!(out, "\n{}", cat.host_no_errors);
        }
        out
    }

    pub(crate) fn open_host_menu(&mut self, host: String, x: u16, y: u16) {
        self.host_inspect = Some(HostInspect {
            host,
            anchor: (x, y),
            selected: 0,
            rects: vec![],
            details: None,
            errors_only: false,
            scroll: 0,
            max_scroll: 0,
            modal: None,
        });
    }
    pub(crate) fn host_inspect_action(&mut self, action: usize) {
        let Some(inspect) = &self.host_inspect else {
            return;
        };
        let report = self.host_diagnostics(&inspect.host, action == 1);
        if action == 2 {
            self.pending_clipboard = Some(report);
            self.show_toast(self.catalog.copied.to_string());
        } else if let Some(inspect) = self.host_inspect.as_mut() {
            inspect.details = Some(report);
            inspect.errors_only = action == 1;
            inspect.scroll = 0;
        }
    }
    pub(crate) fn host_inspect_key(&mut self, key: KeyEvent) {
        let Some(inspect) = &mut self.host_inspect else {
            return;
        };
        self.hover = None;
        let details = inspect.details.is_some();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.host_inspect = None,
            KeyCode::Down | KeyCode::Char('j') => {
                if details {
                    inspect.scroll = inspect.scroll.saturating_add(1).min(inspect.max_scroll)
                } else {
                    inspect.selected = (inspect.selected + 1).min(2)
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if details {
                    inspect.scroll = inspect.scroll.saturating_sub(1)
                } else {
                    inspect.selected = inspect.selected.saturating_sub(1)
                }
            }
            KeyCode::Home if details => inspect.scroll = 0,
            KeyCode::End if details => inspect.scroll = inspect.max_scroll,
            KeyCode::PageDown if details => {
                inspect.scroll = inspect.scroll.saturating_add(10).min(inspect.max_scroll)
            }
            KeyCode::PageUp if details => inspect.scroll = inspect.scroll.saturating_sub(10),
            KeyCode::Enter if !details && !super::super::is_key_repeat(&key) => {
                let action = inspect.selected;
                self.host_inspect_action(action);
            }
            KeyCode::Char('r') if details && !super::super::is_key_repeat(&key) => {
                let action = usize::from(inspect.errors_only);
                self.host_inspect_action(action);
            }
            KeyCode::Char('c') if details && !super::super::is_key_repeat(&key) => {
                let report = inspect.details.clone().unwrap_or_default();
                self.pending_clipboard = Some(report);
                self.show_toast(self.catalog.copied.to_string());
            }
            _ => {}
        }
    }
    pub(crate) fn host_inspect_mouse(&mut self, mouse: MouseEvent) {
        let Some(inspect) = &mut self.host_inspect else {
            return;
        };
        let pos = (mouse.column, mouse.row).into();
        match mouse.kind {
            MouseEventKind::Moved if inspect.details.is_none() => {
                if let Some(selected) = inspect.rects.iter().position(|r| r.contains(pos)) {
                    inspect.selected = selected;
                }
            }
            MouseEventKind::Down(MouseButton::Left) if inspect.details.is_none() => {
                if let Some(action) = inspect.rects.iter().position(|r| r.contains(pos)) {
                    self.host_inspect_action(action);
                } else {
                    self.host_inspect = None;
                }
            }
            MouseEventKind::Down(_) if !inspect.modal.is_some_and(|r| r.contains(pos)) => {
                self.host_inspect = None
            }
            MouseEventKind::ScrollDown => {
                inspect.scroll = inspect.scroll.saturating_add(2).min(inspect.max_scroll)
            }
            MouseEventKind::ScrollUp => inspect.scroll = inspect.scroll.saturating_sub(2),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::remote::tests::{add_remote_workspace, remote_ui_app};
    use ratatui::crossterm::event::KeyModifiers;
    #[test]
    fn host_context_menu_is_read_only_and_keeps_diagnostics_visible() {
        let _env = crate::persist::test_env("host-menu-diagnostics");
        let mut app = remote_ui_app();
        let (view, input, _) = add_remote_workspace(&mut app);
        app.config.layout.workspace_display = crate::config::WorkspaceDisplay::Tree;
        app.config.layout.rounded_corners = true;
        let ViewKind::Remote(v) = app.views.get_mut(&view).unwrap() else {
            panic!()
        };
        v.error = Some("frame read failed: unexpected EOF".into());
        let area = Rect::new(0, 0, 140, 40);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        crate::ui::render_into(
            &mut crate::ui::RenderTarget::new(&mut buffer, area),
            &mut app,
        );
        while input.try_recv().is_ok() {}
        let rect = app
            .workspace_machine_rects
            .iter()
            .find(|(host, _)| host.as_deref() == Some("dev-207"))
            .unwrap()
            .1;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: rect.x + 2,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.host_inspect.as_ref().unwrap().host, "dev-207");
        crate::ui::render_into(
            &mut crate::ui::RenderTarget::new(&mut buffer, area),
            &mut app,
        );
        let rows = app.host_inspect.as_ref().unwrap().rects.clone();
        let motion = |row: Rect| {
            AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: row.x + 1,
                row: row.y,
                modifiers: KeyModifiers::NONE,
            })
        };
        for (selected, row) in rows.iter().copied().enumerate() {
            assert!(app.handle_event(motion(row)), "entering a row must repaint");
            assert_eq!(app.host_inspect.as_ref().unwrap().selected, selected);
            crate::ui::render_into(
                &mut crate::ui::RenderTarget::new(&mut buffer, area),
                &mut app,
            );
            for x in row.x + 1..row.right() - 1 {
                assert_eq!(buffer[(x, row.y)].bg, app.theme.accent);
            }
            assert!(!app.handle_event(motion(row)), "same row is unchanged");
        }
        app.host_inspect_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.host_inspect.as_ref().unwrap().selected, 1);
        assert!(
            app.handle_event(motion(rows[2])),
            "mouse resumes after keyboard selection"
        );
        assert_eq!(app.host_inspect.as_ref().unwrap().selected, 2);
        assert!(app.handle_event(motion(rows[1])));
        assert!(
            input.try_recv().is_err(),
            "hover must not send remote input"
        );
        app.host_inspect_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        crate::ui::render_into(
            &mut crate::ui::RenderTarget::new(&mut buffer, area),
            &mut app,
        );
        let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("unexpected EOF"));
        assert!(!crate::ui::retained_pty_eligible(&app));
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert!(app
            .pending_clipboard
            .as_ref()
            .unwrap()
            .contains("unexpected EOF"));
        assert!(
            input.try_recv().is_err(),
            "inspection must not send input to owner"
        );
        let modal = app.host_inspect.as_ref().unwrap().modal;
        let small = Rect::new(0, 0, 40, 8);
        let mut buffer = ratatui::buffer::Buffer::empty(small);
        crate::ui::render_projection(
            &mut crate::ui::RenderTarget::new(&mut buffer, small),
            &mut app,
        );
        assert_eq!(app.host_inspect.as_ref().unwrap().modal, modal);
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(app.host_inspect.is_none());
    }
}
