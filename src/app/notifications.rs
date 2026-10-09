//! Inbox presentation over the existing bar notification history. No polling,
//! persistence, or second delivery channel; opening is always a user action.
use super::App;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Clone, Default)]
pub(crate) struct InboxUi {
    pub open: bool,
    pub scroll: usize,
    pub selected: Option<u64>,
    pub rect: Rect,
    pub trigger: Rect,
    pub close: Rect,
    pub header: Rect,
    pub clear: Rect,
    pub max_scroll: usize,
    pub rows: Vec<(u64, Rect)>,
    pub cached_lines: Vec<(u64, String, Option<crate::bar::NotificationLevel>)>,
    pub cache_key: Option<(u16, u64)>,
}
impl InboxUi {
    pub fn clear_geometry(&mut self) {
        self.rect = Rect::ZERO;
        self.trigger = Rect::ZERO;
        self.close = Rect::ZERO;
        self.header = Rect::ZERO;
        self.clear = Rect::ZERO;
        self.rows.clear();
    }
}
impl App {
    pub(crate) fn queue_desktop_notification(&mut self, message: String) {
        self.bar
            .record_notification(&message, None, crate::bar::NotificationLevel::Info);
        self.pending_notify.push(message);
    }
    pub(crate) fn notifications_in_inbox(&self) -> bool {
        self.config.notifications.display == crate::config::NotificationDisplay::Inbox
    }
    pub(crate) fn open_notification_inbox(&mut self) {
        self.notification_inbox.open = true;
        self.notification_inbox.scroll = 0;
        self.notification_inbox.selected = self.bar.history.back().map(|item| item.id);
        self.bar.mark_notifications_read();
    }
    pub(crate) fn notification_inbox_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.notification_inbox.open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.notification_inbox.scroll = self.notification_inbox.scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.notification_inbox.scroll =
                    (self.notification_inbox.scroll + 1).min(self.notification_inbox.max_scroll);
            }
            KeyCode::PageUp => {
                self.notification_inbox.scroll = self.notification_inbox.scroll.saturating_sub(10)
            }
            KeyCode::PageDown => {
                self.notification_inbox.scroll =
                    (self.notification_inbox.scroll + 10).min(self.notification_inbox.max_scroll)
            }
            KeyCode::Home => self.notification_inbox.scroll = 0,
            KeyCode::End => self.notification_inbox.scroll = self.notification_inbox.max_scroll,
            KeyCode::Char('r') if key.kind == KeyEventKind::Press => {
                self.bar.mark_notifications_read()
            }
            KeyCode::Char('x') if key.kind == KeyEventKind::Press => {
                self.bar.clear_notification_history();
                self.notification_inbox.scroll = 0;
                self.notification_inbox.selected = None;
            }
            KeyCode::Char('c') if key.kind == KeyEventKind::Press => {
                let selected = self.notification_inbox.selected;
                if let Some(item) = self
                    .bar
                    .history
                    .iter()
                    .find(|item| Some(item.id) == selected)
                {
                    self.pending_clipboard = Some(item.text.clone());
                }
            }
            _ => {}
        }
        // Keyboard scroll makes the first visible entry the copy target; mouse
        // selection keeps its stable ID when newer messages arrive.
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Char('j' | 'k')
        ) {
            self.notification_inbox.selected = None;
        }
    }
    pub(crate) fn notification_inbox_mouse(&mut self, mouse: MouseEvent) -> bool {
        let hit = |r: Rect| {
            mouse.column >= r.x
                && mouse.column < r.right()
                && mouse.row >= r.y
                && mouse.row < r.bottom()
        };
        if hit(self.notification_inbox.trigger) {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if self.notification_inbox.open {
                    self.notification_inbox.open = false;
                } else {
                    self.open_notification_inbox();
                }
            }
            return true;
        }
        if !self.notification_inbox.open {
            return false;
        }
        if !hit(self.notification_inbox.rect) {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.notification_inbox.open = false;
            }
            // Dismiss without clicking the terminal or workspace underneath.
            return true;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if hit(self.notification_inbox.close) || hit(self.notification_inbox.header) {
                    if self.notification_inbox.open {
                        self.notification_inbox.open = false;
                    } else {
                        self.open_notification_inbox();
                    }
                } else if hit(self.notification_inbox.clear) {
                    self.bar.clear_notification_history();
                    self.notification_inbox.scroll = 0;
                    self.notification_inbox.selected = None;
                } else if let Some((id, _)) = self
                    .notification_inbox
                    .rows
                    .iter()
                    .find(|(_, rect)| hit(*rect))
                {
                    self.notification_inbox.selected = Some(*id);
                    if let Some(item) = self.bar.history.iter_mut().find(|item| item.id == *id) {
                        item.unread = false;
                        self.bar.history_revision = self.bar.history_revision.wrapping_add(1);
                    }
                } else if !self.notification_inbox.open {
                    self.open_notification_inbox();
                }
            }
            MouseEventKind::ScrollUp if self.notification_inbox.open => {
                self.notification_inbox.scroll = self.notification_inbox.scroll.saturating_sub(3);
                self.notification_inbox.selected = None;
            }
            MouseEventKind::ScrollDown if self.notification_inbox.open => {
                self.notification_inbox.scroll =
                    (self.notification_inbox.scroll + 3).min(self.notification_inbox.max_scroll);
                self.notification_inbox.selected = None;
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::{BarState, NotificationLevel, NotificationPush};
    use crate::event::AppEvent;
    use crossterm::event::KeyModifiers;
    use std::time::{Duration, Instant};

    #[test]
    fn none_theme_keeps_terminal_defaults_after_light_and_dark_probes() {
        let _env = crate::persist::test_env("none-terminal-theme");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        assert!(app.apply_theme_locally("none"));
        for (fg, bg) in [
            ([30, 40, 50], [240, 241, 242]),
            ([230, 231, 232], [20, 21, 22]),
        ] {
            let colors = crate::terminal::theme_probe::TerminalColors {
                fg,
                bg,
                palette: crate::terminal::theme_probe::default_ansi_palette(fg, bg),
            };
            app.apply_terminal_colors(&colors);
            for color in [
                app.theme.base,
                app.theme.mantle,
                app.theme.crust,
                app.theme.surface0,
                app.theme.surface1,
                app.theme.sel_bg,
                app.theme.text,
            ] {
                assert_eq!(color, ratatui::style::Color::Reset);
            }
            assert_eq!(app.pane_appearance.foreground, Some(fg));
            assert_eq!(app.pane_appearance.background, Some(bg));
            assert_eq!(app.config.theme, "none");
        }
    }

    #[test]
    fn chrome_uses_readable_theme_surfaces_without_repainting_none_canvas() {
        use ratatui::style::{Color, Modifier};
        let _env = crate::persist::test_env("chrome-surfaces");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(140, 40, tx).unwrap();
        app.config.layout.rounded_corners = true;
        app.apply_theme_locally("none");
        let area = Rect::new(0, 0, 140, 40);
        let draw = |app: &mut App| {
            let mut b = ratatui::buffer::Buffer::empty(area);
            crate::ui::render_into(&mut crate::ui::RenderTarget::new(&mut b, area), app);
            b
        };
        for (fg, bg) in [([30, 40, 50], [245; 3]), ([230; 3], [20; 3])] {
            let mut colors = crate::terminal::theme_probe::TerminalColors {
                fg,
                bg,
                palette: crate::terminal::theme_probe::default_ansi_palette(fg, bg),
            };
            colors.palette[4] = [40, 75, 180];
            app.apply_terminal_colors(&colors);
            let theme = app.theme.chrome(Some(&colors));
            let b = draw(&mut app);
            assert_eq!(b[(80, 12)].bg, Color::Reset);
            assert_eq!(b[(70, 39)].bg, theme.crust);
            assert_ne!(theme.crust, Color::Reset);
            let menu = app.settings_icon_rect.unwrap();
            assert_eq!(b[(menu.x, menu.y)].symbol(), "◖");
            assert_eq!(b[(menu.right() - 1, menu.y)].symbol(), "◗");
            app.hover = Some((menu.x + 1, menu.y));
            let b = draw(&mut app);
            assert_eq!(b[(menu.x + 1, menu.y)].bg, theme.accent);
            assert_eq!(b[(menu.x + 1, menu.y)].fg, Color::Rgb(255, 255, 255));
            let row = app.ws_rects[0].1;
            app.hover = Some((row.x + 2, row.y));
            let b = draw(&mut app);
            assert_eq!(b[(row.x + 2, row.y)].bg, theme.surface0);
            assert!(!b[(row.x + 2, row.y)]
                .modifier
                .contains(Modifier::UNDERLINED));
            app.hover = None;
        }
    }

    #[test]
    fn notification_history_survives_ttl_coalesces_and_stays_bounded() {
        let mut bar = BarState::default();
        let now = Instant::now();
        for _ in 0..2 {
            bar.push_notification(
                NotificationPush {
                    owner: None,
                    text: "Task ready".into(),
                    level: NotificationLevel::Success,
                    ttl_ms: 500,
                    action: None,
                    value: None,
                    dedupe_key: Some("task".into()),
                },
                now,
            )
            .unwrap();
        }
        assert_eq!(bar.notifications.len(), 1);
        assert_eq!(bar.history.len(), 1);
        assert_eq!(bar.history[0].count, 2);
        assert!(bar.tick(now + Duration::from_secs(1)));
        assert!(bar.notifications.is_empty());
        assert_eq!(bar.history[0].text, "Task ready");
        bar.mark_notifications_read();
        assert!(!bar.history[0].unread);
        bar.record_notification("Task ready", None, NotificationLevel::Success);
        assert!(bar.history[0].unread);
        for i in 0..120 {
            bar.record_notification(&format!("message {i}"), None, NotificationLevel::Info);
        }
        assert_eq!(bar.history.len(), 100);
        assert_eq!(
            bar.pending_notifications.len(),
            crate::bar::MAX_NOTIFICATIONS
        );
        bar.record_notification(&"界\x1b".repeat(3000), None, NotificationLevel::Info);
        assert!(bar.history.back().unwrap().text.len() <= 4096);
        assert!(!bar.history.back().unwrap().text.contains('\x1b'));
        let len = bar.pending_notifications.len();
        bar.ingest_notification(
            "remote",
            Some("host / session"),
            NotificationLevel::Info,
            false,
        );
        assert_eq!(bar.pending_notifications.len(), len);
    }

    #[test]
    fn inbox_preview_expires_without_losing_history_or_closing_the_popup() {
        let _env = crate::persist::test_env("inbox-preview-expiry");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(140, 40, tx).unwrap();
        app.config.notifications.display = crate::config::NotificationDisplay::Inbox;
        let area = Rect::new(0, 0, 140, 40);
        let draw = |app: &mut App| {
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            crate::ui::render_into(&mut crate::ui::RenderTarget::new(&mut buffer, area), app);
            (0..area.width)
                .map(|x| buffer[(x, area.bottom() - 1)].symbol())
                .collect::<String>()
        };
        for remote in [false, true] {
            if remote {
                app.bar.ingest_notification(
                    "REMOTE_PREVIEW",
                    Some("host / session"),
                    NotificationLevel::Info,
                    false,
                );
            } else {
                app.show_toast("LOCAL_PREVIEW");
            }
            let label = if remote {
                "REMOTE_PREVIEW"
            } else {
                "LOCAL_PREVIEW"
            };
            let until = app.bar.notification_preview_until.unwrap();
            assert!(draw(&mut app).contains(label));
            assert!(!app.tick_bar_notifications(until - Duration::from_millis(1)));
            assert!(app.tick_bar_notifications(until));
            assert!(!draw(&mut app).contains(label));
            assert!(app.notification_inbox.trigger.width < 10);
            assert!(app.bar.history.back().unwrap().unread);
            let trigger = app.notification_inbox.trigger;
            assert!(app.notification_inbox_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: trigger.x + 1,
                row: trigger.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(app.notification_inbox.open);
            assert_eq!(app.bar.history.back().unwrap().text, label);
            assert!(!app.tick_bar_notifications(until + Duration::from_secs(30)));
            assert!(app.notification_inbox.open);
            app.notification_inbox.open = false;
        }
        app.show_toast("REPEAT_PREVIEW");
        let until = app.bar.notification_preview_until.unwrap();
        app.tick_bar_notifications(until);
        app.show_toast("REPEAT_PREVIEW");
        assert_eq!(app.bar.history.back().unwrap().count, 2);
        assert!(draw(&mut app).contains("REPEAT_PREVIEW"));
        app.bar.clear_notification_history();
        assert!(app.bar.notification_preview_until.is_none());
        draw(&mut app);
        assert!(app.notification_inbox.trigger.is_empty());
    }

    #[test]
    fn notification_inbox_is_optional_quiet_and_passive_geometry_safe() {
        let _env = crate::persist::test_env("notification-inbox");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(140, 40, tx).unwrap();
        app.show_toast("legacy feedback");
        assert!(app.toast.is_some());
        app.config.notifications.display = crate::config::NotificationDisplay::Inbox;
        app.show_toast("quiet feedback");
        assert!(app.toast.is_none());
        assert!(!app.notification_inbox.open);
        let area = Rect::new(0, 0, 140, 40);
        let draw = |app: &mut App| {
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            crate::ui::render_into(&mut crate::ui::RenderTarget::new(&mut buffer, area), app);
            buffer
        };
        draw(&mut app);
        let panes = app.pane_content_rects.clone();
        let header = app.notification_inbox.trigger;
        assert!(!header.is_empty());
        assert_eq!(header.y, area.bottom() - 1);
        assert!(app.notification_inbox.rect.is_empty());
        let agents = app.agents_area;
        let workspaces = app.workspaces_area;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: header.x + 1,
            row: header.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.notification_inbox.open);
        draw(&mut app);
        assert_eq!(app.pane_content_rects, panes);
        assert_eq!(app.agents_area, agents);
        assert_eq!(app.workspaces_area, workspaces);
        assert_eq!(app.notification_inbox.rect.bottom(), header.y);
        assert!(app.notification_inbox.rect.width > agents.width);
        let trigger = app.notification_inbox.trigger;
        assert!(app.bar.history.iter().all(|r| !r.unread));
        let geometry = app.notification_inbox.rows.clone();
        let other = Rect::new(0, 0, 68, 18);
        let mut buffer = ratatui::buffer::Buffer::empty(other);
        crate::ui::render_projection(
            &mut crate::ui::RenderTarget::new(&mut buffer, other),
            &mut app,
        );
        assert_eq!(app.notification_inbox.rows, geometry);
        assert_eq!(app.notification_inbox.trigger, trigger);
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.pending_clipboard.as_deref(), Some("quiet feedback"));
        app.show_toast("while reading");
        assert!(app.bar.history.back().unwrap().unread);
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(!app.notification_inbox.open);
        app.open_notification_inbox();
        draw(&mut app);
        let focus = app.layout().focus;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 139,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!app.notification_inbox.open);
        assert_eq!(app.layout().focus, focus);
        // Collapsed and narrow render paths remain bounded, including no workspace.
        app.workspaces.clear();
        let small = Rect::new(0, 0, 24, 6);
        let mut buffer = ratatui::buffer::Buffer::empty(small);
        crate::ui::render_into(
            &mut crate::ui::RenderTarget::new(&mut buffer, small),
            &mut app,
        );
    }
}
