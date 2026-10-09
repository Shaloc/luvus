//! Quiet sidebar notification history; only explicit opening expands it.
use super::{theme::Theme, RenderTarget};
use crate::{app::App, bar::NotificationLevel};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

/// Reserve a footer in the chosen existing sidebar. Expanded history occupies
/// only its dock body, keeping the terminal and its PTY size untouched.
pub(super) fn sidebar(f: &mut RenderTarget, body: Rect, app: &mut App, t: &Theme) -> Rect {
    if body.width < 3 || body.height == 0 {
        return body;
    }
    let height = if app.notification_inbox.open {
        body.height
    } else {
        body.height.min(3)
    };
    let area = Rect::new(
        body.x,
        body.bottom() - height,
        body.width.saturating_sub(1),
        height,
    );
    draw(f, area, app, t);
    Rect::new(body.x, body.y, body.width, body.height - height)
}
fn level_color(level: NotificationLevel, t: &Theme) -> ratatui::style::Color {
    match level {
        NotificationLevel::Info => t.accent,
        NotificationLevel::Success => t.mint,
        NotificationLevel::Warning => t.amber,
        NotificationLevel::Error => t.coral,
    }
}
pub(super) fn draw(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    if area.width < 3 || area.height == 0 {
        return;
    }
    let cat = app.catalog.settings;
    let unread = app.bar.history.iter().filter(|item| item.unread).count();
    let open = app.notification_inbox.open;
    app.notification_inbox.rect = area;
    app.notification_inbox.header = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(Block::new().style(Style::new().bg(t.base)), area);
    let header = format!(
        "{} {} {} / {}",
        if open { "▾" } else { "▸" },
        cat.notify_inbox,
        unread,
        app.bar.history.len()
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            super::truncate(&header, area.width as usize),
            Style::new()
                .fg(if unread > 0 { t.accent } else { t.subtext0 })
                .bg(t.surface0)
                .bold(),
        )),
        app.notification_inbox.header,
    );
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    if !open {
        let text = app
            .bar
            .history
            .back()
            .map(|item| item.text.lines().next().unwrap_or(""))
            .unwrap_or(cat.notify_empty);
        f.render_widget(
            Paragraph::new(super::truncate(text, body.width as usize))
                .style(Style::new().fg(t.subtext0)),
            body,
        );
        return;
    }
    let body = Rect::new(body.x, body.y, body.width, body.height.saturating_sub(1));
    let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    app.notification_inbox.clear = Rect::new(
        footer.right().saturating_sub(3),
        footer.y,
        3.min(footer.width),
        1,
    );
    f.render_widget(
        Paragraph::new(super::truncate(
            cat.notify_hint,
            footer.width.saturating_sub(3) as usize,
        ))
        .style(Style::new().fg(t.overlay1)),
        footer,
    );
    f.render_widget(
        Paragraph::new(" × ").style(Style::new().fg(t.coral)),
        app.notification_inbox.clear,
    );
    if app.bar.history.is_empty() {
        f.render_widget(
            Paragraph::new(cat.notify_empty).style(Style::new().fg(t.overlay1)),
            body,
        );
        app.notification_inbox.max_scroll = 0;
        return;
    }
    // At most 100 bounded messages; wrap on the current display-cell width.
    // No I/O, timers or terminal-grid copies are involved.
    let key = (body.width, app.bar.history_revision);
    if app.notification_inbox.cache_key != Some(key) {
        let mut lines = Vec::new();
        for item in app.bar.history.iter().rev() {
            let seconds = item
                .time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let time = format!(
                "{:02}:{:02}:{:02}Z",
                seconds / 3600 % 24,
                seconds / 60 % 60,
                seconds % 60
            );
            let label = format!(
                "{} {} {}{}",
                if item.unread { "●" } else { "○" },
                time,
                item.owner.as_deref().unwrap_or("Luvus"),
                if item.count > 1 {
                    format!(" ×{}", item.count)
                } else {
                    String::new()
                }
            );
            // Keep host/session legible in a narrow sidebar: it gets its own
            // line instead of losing its suffix after the timestamp.
            if super::display_width(&label) > body.width as usize {
                let (stamp, source) = label.split_at(label.find('Z').unwrap() + 1);
                for part in [stamp, source.trim()] {
                    for line in super::board::wrap_display_lines(part, body.width as usize) {
                        lines.push((item.id, line, Some(item.level)));
                    }
                }
            } else {
                lines.push((item.id, label, Some(item.level)));
            }
            for line in super::board::wrap_display_lines(&item.text, body.width as usize) {
                lines.push((item.id, line, None));
            }
            lines.push((item.id, String::new(), None));
        }
        app.notification_inbox.cached_lines = lines;
        app.notification_inbox.cache_key = Some(key);
    }
    if app
        .notification_inbox
        .selected
        .is_some_and(|id| !app.bar.history.iter().any(|item| item.id == id))
    {
        app.notification_inbox.selected = None;
    }
    let lines = &app.notification_inbox.cached_lines;
    app.notification_inbox.max_scroll = lines.len().saturating_sub(body.height as usize);
    app.notification_inbox.scroll = app
        .notification_inbox
        .scroll
        .min(app.notification_inbox.max_scroll);
    let visible = lines
        .iter()
        .skip(app.notification_inbox.scroll)
        .take(body.height as usize);
    for (row, (id, line, level)) in visible.enumerate() {
        if app.notification_inbox.selected.is_none() {
            app.notification_inbox.selected = Some(*id);
        }
        let rect = Rect::new(body.x, body.y + row as u16, body.width, 1);
        let selected = app.notification_inbox.selected == Some(*id);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                line.as_str(),
                level
                    .map(|v| Style::new().fg(level_color(v, t)).bold())
                    .unwrap_or_default(),
            )))
            .style(Style::new().fg(t.text).bg(if selected {
                t.surface0
            } else {
                t.base
            })),
            rect,
        );
        app.notification_inbox.rows.push((*id, rect));
    }
}
