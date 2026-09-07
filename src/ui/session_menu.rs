use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::session_menu::{NamedSessionPrompt, RemotePromptField};
use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::{display_width, truncate, RenderTarget};

const MOBILE_ITEM_HEIGHT: u16 = 2;

pub(crate) fn draw_session_menu(
    f: &mut RenderTarget,
    viewport: Rect,
    app: &mut App,
    theme: &Theme,
) {
    app.named_session_row_rects.clear();
    let (loading, cursor, mut scroll, prompt, error, preparing, item_count) = {
        let Some(menu) = app.named_session_menu.as_ref() else {
            return;
        };
        (
            menu.loading,
            menu.cursor,
            menu.scroll,
            menu.prompt.clone(),
            menu.error.clone(),
            menu.preparing,
            menu.rows.len() + 2,
        )
    };
    let mobile = app.compact;
    let item_height = if mobile { MOBILE_ITEM_HEIGHT } else { 1 };
    let prompt_height = match prompt.as_ref() {
        Some(NamedSessionPrompt::Local { .. }) => 5,
        Some(NamedSessionPrompt::Remote { .. }) => 7,
        None => 0,
    };
    let area = if mobile {
        viewport
    } else {
        let anchor = app
            .named_session_button_rect
            .unwrap_or(Rect::new(viewport.x, viewport.y, 24, 1));
        // Fill outward from whichever sidebar owns the selector. This removes
        // the dead strip left by anchoring the popup to the label itself and
        // lets a right-only layout open the same menu toward the pane area.
        // Size from the same labels we render: multiple owner statuses must
        // not squeeze out the name while the viewport still has room.
        let desired_width = app.named_session_menu.as_ref().map_or(64, |menu| {
            menu.rows
                .iter()
                .map(|row| {
                    display_width(row.display_name()) + display_width(&session_state(app, row)) + 8
                })
                .max()
                .unwrap_or(64)
                .max(64)
        });
        let width = desired_width.min(viewport.width.max(1) as usize) as u16;
        let on_right = anchor.x >= viewport.x.saturating_add(viewport.width / 2);
        let x = if on_right {
            viewport.right().saturating_sub(width)
        } else {
            viewport.x
        };
        let desired = if prompt.is_some() {
            2 + prompt_height
        } else {
            (item_count as u16)
                .saturating_mul(item_height)
                .saturating_add(3 + u16::from(error.is_some()))
        };
        let height = desired.min(viewport.height.saturating_sub(1)).max(3);
        Rect::new(x, anchor.bottom(), width, height)
    };
    app.named_session_menu_rect = Some(area);

    f.render_widget(Clear, area);
    f.render_widget(
        Block::new().style(Style::new().bg(theme.surface0).fg(theme.text)),
        area,
    );
    let block = Block::new()
        .borders(Borders::ALL)
        .title(format!(" {} ", app.catalog.named_sessions.to_uppercase()))
        .border_style(Style::new().fg(theme.border_focus).bg(theme.surface0))
        .style(Style::new().bg(theme.surface0));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let close_width = if mobile {
        12.min(area.width)
    } else {
        3.min(area.width)
    };
    let close_height = if mobile { 2.min(area.height) } else { 1 };
    let close = Rect::new(
        area.right().saturating_sub(close_width + 1),
        area.y,
        close_width,
        close_height,
    );
    app.named_session_close_rect = Some(close);
    let close_hovered = app.hover.is_some_and(|(x, y)| contains(close, x, y));
    let close_style = if close_hovered {
        Style::new().fg(theme.crust).bg(theme.accent).bold()
    } else {
        Style::new().fg(theme.accent).bg(theme.surface0).bold()
    };
    let close_text = if mobile {
        vec![
            Line::from(app.catalog.act_close.to_uppercase()),
            Line::from(app.catalog.named_sessions.to_uppercase()),
        ]
    } else {
        vec![Line::from(" × ")]
    };
    f.render_widget(
        Paragraph::new(close_text)
            .alignment(Alignment::Center)
            .style(close_style),
        close,
    );

    // The mobile close control uses the title row plus the first inner row as
    // a two-row touch target. Keep all sheet content below that full target so
    // rendering and hit testing cannot overlap it.
    let content_top = inner.y.max(close.bottom());
    let content = Rect::new(
        inner.x,
        content_top,
        inner.width,
        inner.bottom().saturating_sub(content_top),
    );

    if let Some(prompt) = prompt.as_ref() {
        draw_prompt(f, content, app, prompt, preparing, error.as_deref(), theme);
        return;
    }

    if loading {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", app.catalog.session_loading),
                Style::new().fg(theme.overlay1),
            )),
            content,
        );
        return;
    }
    if let Some(error) = error.as_deref() {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(error, inner.width as usize),
                Style::new().fg(theme.coral),
            )),
            Rect::new(content.x, content.y, content.width, 1),
        );
    }

    let top = content.y + u16::from(error.is_some());
    let available = content.bottom().saturating_sub(top + 1) as usize;
    f.render_widget(
        Paragraph::new(format!(
            " r {} · d {} · {} [{}]",
            app.catalog.act_refresh,
            app.catalog.act_delete,
            app.catalog.session_merge,
            if app.remote_merge_enabled { "✓" } else { " " }
        ))
        .style(Style::new().fg(theme.overlay1)),
        Rect::new(
            content.x,
            content.bottom().saturating_sub(1),
            content.width,
            1,
        ),
    );
    let visible_items = available / item_height as usize;
    if cursor < scroll {
        scroll = cursor;
    } else if visible_items > 0 && cursor >= scroll + visible_items {
        scroll = cursor + 1 - visible_items;
    }
    scroll = scroll.min(item_count.saturating_sub(visible_items.max(1)));
    if let Some(menu) = app.named_session_menu.as_mut() {
        menu.scroll = scroll;
    }

    for index in scroll..item_count.min(scroll + visible_items) {
        let y = top + ((index - scroll) as u16 * item_height);
        let rect = Rect::new(inner.x, y, inner.width, item_height);
        let hovered =
            app.session_menu.is_none() && app.hover.is_some_and(|(x, y)| contains(rect, x, y));
        let selected = index == cursor;
        let hot = selected || hovered;
        if hot {
            f.render_widget(Block::new().style(Style::new().bg(theme.accent)), rect);
        }
        draw_row(f, rect, app, index, selected, hot, theme);
        app.named_session_row_rects.push((index, rect));
    }
}

fn draw_prompt(
    f: &mut RenderTarget,
    area: Rect,
    app: &App,
    prompt: &NamedSessionPrompt,
    preparing: bool,
    error: Option<&str>,
    theme: &Theme,
) {
    if let NamedSessionPrompt::Remote {
        name,
        hosts,
        host_index,
        focus,
    } = prompt
    {
        draw_remote_prompt(
            f,
            area,
            app,
            name,
            hosts.get(*host_index).map_or("", String::as_str),
            *focus,
            preparing,
            error,
            theme,
        );
        return;
    }
    let NamedSessionPrompt::Local { name: prompt } = prompt else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let label = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(
        Paragraph::new(Span::styled(
            app.catalog.session_name,
            Style::new().fg(theme.text).bold(),
        )),
        label,
    );
    if area.height > 1 {
        let input = Rect::new(area.x, area.y + 1, area.width, 1);
        let suffix = if preparing { " …" } else { "▏" };
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!("{prompt}{suffix}"), input.width as usize),
                Style::new().fg(theme.text).bg(theme.surface0),
            )),
            input,
        );
    }
    if area.height > 2 {
        let message = error.unwrap_or(app.catalog.session_name_hint);
        let color = if error.is_some() {
            theme.coral
        } else {
            theme.overlay1
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(message, area.width as usize),
                Style::new().fg(color),
            )),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
    if area.height > 3 {
        f.render_widget(
            Paragraph::new(Span::styled(
                "⏎  enter   esc  back",
                Style::new().fg(theme.overlay0),
            )),
            Rect::new(area.x, area.y + 3, area.width, 1),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_remote_prompt(
    f: &mut RenderTarget,
    area: Rect,
    app: &App,
    name: &str,
    host: &str,
    focus: RemotePromptField,
    preparing: bool,
    error: Option<&str>,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    let selectable = |selected: bool| {
        if selected {
            Style::new().fg(theme.crust).bg(theme.accent).bold()
        } else {
            Style::new().fg(theme.text).bg(theme.surface0)
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(
                &format!("{}: ‹ {} ›", app.catalog.session_host, host),
                area.width as usize,
            ),
            selectable(focus == RemotePromptField::Host),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height > 1 {
        f.render_widget(
            Paragraph::new(Span::styled(
                app.catalog.session_name,
                if focus == RemotePromptField::Name {
                    Style::new().fg(theme.accent).bold()
                } else {
                    Style::new().fg(theme.text).bold()
                },
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
    if area.height > 2 {
        let suffix = if preparing { " …" } else { "▏" };
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!("{name}{suffix}"), area.width as usize),
                Style::new().fg(theme.text).bg(theme.surface0),
            )),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
    if area.height > 3 {
        let state = if app.remote_merge_enabled {
            app.catalog.session_on
        } else {
            app.catalog.session_off
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(
                    &format!("{}: {}", app.catalog.session_merge, state),
                    area.width as usize,
                ),
                Style::new().fg(theme.overlay1),
            )),
            Rect::new(area.x, area.y + 3, area.width, 1),
        );
    }
    if area.height > 4 {
        let message = error.unwrap_or(app.catalog.session_name_hint);
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(message, area.width as usize),
                Style::new().fg(if error.is_some() {
                    theme.coral
                } else {
                    theme.overlay1
                }),
            )),
            Rect::new(area.x, area.y + 4, area.width, 1),
        );
    }
    if area.height > 5 {
        f.render_widget(
            Paragraph::new(Span::styled(
                app.catalog.settings.remote_prompt_hint,
                Style::new().fg(theme.overlay0),
            )),
            Rect::new(area.x, area.y + 5, area.width, 1),
        );
    }
}

fn draw_row(
    f: &mut RenderTarget,
    rect: Rect,
    app: &App,
    index: usize,
    selected: bool,
    hot: bool,
    theme: &Theme,
) {
    let arrow = if selected { "▸" } else { " " };
    let width = rect.width as usize;
    if index == 0 {
        let label = format!("{arrow} + {}", app.catalog.new_session);
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&label, width),
                Style::new()
                    .fg(if hot { theme.crust } else { theme.accent })
                    .bold(),
            )),
            rect,
        );
        if rect.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("    {}", app.catalog.session_name),
                    Style::new().fg(if hot { theme.crust } else { theme.overlay0 }),
                )),
                Rect::new(rect.x, rect.y + 1, rect.width, 1),
            );
        }
        return;
    }
    if index == 1 {
        let label = format!("{arrow} + {}", app.catalog.new_remote_session);
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&label, width),
                Style::new()
                    .fg(if hot { theme.crust } else { theme.accent })
                    .bold(),
            )),
            Rect::new(rect.x, rect.y, rect.width, 1),
        );
        if rect.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(
                        "    {} · {}",
                        app.catalog.session_host, app.catalog.session_name
                    ),
                    Style::new().fg(if hot { theme.crust } else { theme.overlay0 }),
                )),
                Rect::new(rect.x, rect.y + 1, rect.width, 1),
            );
        }
        return;
    }
    let Some(row) = app
        .named_session_menu
        .as_ref()
        .and_then(|menu| menu.rows.get(index - 2))
    else {
        return;
    };
    let state = session_state(app, row);
    let dot = if row.remote.is_some() {
        "◆"
    } else if row.running {
        "●"
    } else {
        "○"
    };
    let state_width = if rect.height > 1 {
        0
    } else {
        display_width(&state) + 2
    };
    // Even a many-host status must not erase the session's identity.
    let name_width = width
        .saturating_sub(state_width + 4)
        .max(width.saturating_sub(4).min(16));
    let mut spans = vec![
        Span::styled(
            format!("{arrow} {dot} "),
            Style::new().fg(if hot { theme.crust } else { theme.accent }),
        ),
        Span::styled(
            truncate(row.display_name(), name_width),
            Style::new()
                .fg(if hot {
                    theme.crust
                } else if row.current {
                    theme.accent
                } else {
                    theme.text
                })
                .bold(),
        ),
    ];
    if rect.height == 1 {
        spans.push(Span::styled(
            format!("  {state}"),
            Style::new().fg(if hot {
                theme.crust
            } else if row.running {
                theme.green
            } else {
                theme.overlay0
            }),
        ));
    }
    let primary = Line::from(spans);
    f.render_widget(
        Paragraph::new(primary),
        Rect::new(rect.x, rect.y, rect.width, 1),
    );
    if rect.height > 1 {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("    {state}"),
                Style::new().fg(if hot {
                    theme.crust
                } else if row.running {
                    theme.green
                } else {
                    theme.overlay0
                }),
            )),
            Rect::new(rect.x, rect.y + 1, rect.width, 1),
        );
    }
}

fn session_state(app: &App, row: &crate::app::session_menu::NamedSessionRow) -> String {
    let mut state = if let Some(remote) = &row.remote {
        let status = if app
            .remote_host_status
            .iter()
            .any(|host| host.host == remote.host && host.error.is_some())
        {
            "?"
        } else if row.running {
            app.catalog.session_running
        } else {
            app.catalog.session_stopped
        };
        if row.current {
            format!(
                "{} · {} · {} · {}",
                app.catalog.session_current, app.catalog.remote_session, remote.host, status
            )
        } else {
            format!(
                "{} · {} · {}",
                app.catalog.remote_session, remote.host, status
            )
        }
    } else if row.current {
        format!(
            "{} · {}",
            app.catalog.session_current, app.catalog.session_running
        )
    } else if row.running {
        app.catalog.session_running.to_string()
    } else {
        app.catalog.session_stopped.to_string()
    };
    if row.merged && row.remote.is_none() {
        let mut first_host = true;
        for host in &app.remote_host_status {
            let session = host
                .sessions
                .iter()
                .find(|session| session.name == row.display_name());
            let cached_owner = app.named_session_menu.as_ref().is_some_and(|menu| {
                menu.remote_targets
                    .iter()
                    .any(|target| target.host == host.host && target.session == row.display_name())
            });
            if session.is_some() || (host.error.is_some() && cached_owner) {
                if first_host {
                    state = format!("{} · {}", app.catalog.session_local, state);
                    first_host = false;
                }
                let status = if host.error.is_some() {
                    "?"
                } else if session.is_some_and(|session| session.running) {
                    app.catalog.session_running
                } else {
                    app.catalog.session_stopped
                };
                state.push_str(&format!(" [{}: {}]", host.host, status));
            }
        }
    }
    state
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::draw_session_menu;
    use crate::app::session_menu::{NamedSessionMenu, NamedSessionRow};
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    fn menu() -> NamedSessionMenu {
        NamedSessionMenu {
            client_id: None,
            remote_targets: Vec::new(),
            generation: 1,
            rows: vec![
                NamedSessionRow {
                    name: "default".into(),
                    running: true,
                    current: true,
                    remote: None,
                    merged: false,
                },
                NamedSessionRow {
                    name: "review".into(),
                    running: false,
                    current: false,
                    remote: None,
                    merged: false,
                },
            ],
            hosts: vec!["dev-207".into()],
            cursor: 2,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        }
    }

    fn rendered_row(app: &App, height: u16) -> String {
        let area = Rect::new(0, 0, 160, height);
        let mut buffer = Buffer::empty(area);
        super::draw_row(
            &mut crate::ui::RenderTarget::new(&mut buffer, area),
            area,
            app,
            3,
            false,
            false,
            &app.theme,
        );
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn remote_session_rows_show_running_and_stopped_on_desktop_and_mobile() {
        let _env = crate::persist::test_env("remote-session-state-labels");
        let mut app = crate::app::remote::tests::remote_ui_app();
        let target = crate::session::remote::RemoteSession::new("build", "review").unwrap();
        let mut menu = menu();
        menu.rows[1].name = target.canonical_name();
        menu.rows[1].remote = Some(target);
        app.named_session_menu = Some(menu);
        for running in [false, true] {
            app.named_session_menu.as_mut().unwrap().rows[1].running = running;
            for height in [1, 2] {
                let text = rendered_row(&app, height);
                assert!(text.contains("remote · build"), "{text}");
                assert!(
                    text.contains(if running { "running" } else { "stopped" }),
                    "{text}"
                );
                assert!(
                    !text.contains("remote-build-review"),
                    "only the actual session name is displayed"
                );
            }
        }
        app.remote_host_status = vec![crate::session::remote::HostStatus {
            host: "build".into(),
            error: Some("offline".into()),
            sessions: Vec::new(),
        }];
        app.named_session_menu.as_mut().unwrap().rows[1].running = false;
        let text = rendered_row(&app, 1);
        assert!(text.contains("remote · build · ?"));
        assert!(
            !text.contains("stopped"),
            "failed discovery is not proof that an owner stopped"
        );
    }

    #[test]
    fn merged_session_rows_distinguish_local_and_remote_states() {
        let _env = crate::persist::test_env("merged-session-state-labels");
        let mut app = crate::app::remote::tests::remote_ui_app();
        let mut menu = menu();
        menu.rows[1].merged = true;
        app.named_session_menu = Some(menu);
        app.remote_host_status = vec![crate::session::remote::HostStatus {
            host: "build".into(),
            error: None,
            sessions: vec![crate::session::remote::HostSession {
                name: "review".into(),
                running: true,
            }],
        }];
        for height in [1, 2] {
            let text = rendered_row(&app, height);
            assert!(
                text.contains(&format!("{} · stopped", app.catalog.session_local)),
                "{text}"
            );
            assert!(text.contains("[build: running]"), "{text}");
        }
        app.remote_host_status[0].sessions[0].running = false;
        assert!(rendered_row(&app, 1).contains("[build: stopped]"));

        // Discovery failures return no live inventory but retain cached targets.
        app.named_session_menu.as_mut().unwrap().remote_targets =
            vec![crate::session::remote::RemoteSession::new("build", "review").unwrap()];
        app.remote_host_status[0].error = Some("offline".into());
        app.remote_host_status[0].sessions.clear();
        for height in [1, 2] {
            assert!(rendered_row(&app, height).contains("[build: ?]"));
        }
        app.named_session_menu
            .as_mut()
            .unwrap()
            .remote_targets
            .clear();
        assert!(!rendered_row(&app, 1).contains("[build:"));
    }

    #[test]
    fn desktop_header_shows_the_canonical_session_and_menu_rows() {
        let _env = crate::persist::test_env("named-session-menu-render");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.server_mode = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        let session = app.named_session_button_rect.unwrap();
        let toggle = app.sidebar_toggle_rect.unwrap();
        assert_eq!(session.x, toggle.right() + 2);
        assert_ne!(
            terminal
                .backend()
                .buffer()
                .cell((session.x, session.y))
                .unwrap()
                .bg,
            app.theme.accent,
            "the idle label has no accent block"
        );

        app.hover = Some((session.x, session.y));
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((session.x, session.y))
                .unwrap()
                .bg,
            app.theme.accent,
            "hovering turns the label into an accent block"
        );
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((session.x, session.y))
                .unwrap()
                .symbol(),
            " "
        );
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((session.right() - 1, session.y))
                .unwrap()
                .symbol(),
            " "
        );
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((session.right() - 1, session.y))
                .unwrap()
                .bg,
            app.theme.accent,
            "the hover block keeps right-side padding"
        );

        app.named_session_menu = Some(menu());
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        let top: String = (0..app.sidebars.left.width)
            .map(|x| terminal.backend().buffer().cell((x, 0)).unwrap().symbol())
            .collect();
        assert!(top.contains(" default "));
        assert!(!top.contains('↔'));
        assert!(!top.contains('▾'));
        assert!(!top.contains("luvus"));
        assert_eq!(app.named_session_row_rects.len(), 4);
        assert!(app.named_session_button_rect.is_some());
        assert!(
            app.named_session_menu_rect.unwrap().right() > app.sidebars.left.width,
            "the popup overlays the pane instead of clipping to the sidebar"
        );
        let popup = app.named_session_menu_rect.unwrap();
        assert_eq!(popup.x, 0, "the popup is flush with the left sidebar");
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((popup.x, popup.y + 1))
                .unwrap()
                .fg,
            app.theme.border_focus,
            "the session popup uses the context-menu border"
        );
        let selected = app
            .named_session_row_rects
            .iter()
            .find(|(index, _)| *index == 2)
            .unwrap()
            .1;
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((selected.x, selected.y))
                .unwrap()
                .bg,
            app.theme.accent,
            "selected rows use the context-menu accent block"
        );
        let selected_text: String = (selected.x..selected.right())
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, selected.y))
                    .unwrap()
                    .symbol()
            })
            .collect();
        assert!(selected_text.starts_with("▸ ● default"));
    }

    #[test]
    fn desktop_selector_and_popup_follow_a_right_only_sidebar() {
        let _env = crate::persist::test_env("named-session-menu-right-sidebar");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.server_mode = true;
        app.sidebars.left.visible = false;
        app.sidebars.right.visible = true;
        app.sidebars.right.docks = std::mem::take(&mut app.sidebars.left.docks);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();

        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let session = app.named_session_button_rect.unwrap();
        let menu_button = app.settings_icon_rect.unwrap();
        assert!(session.x >= 120 - app.sidebars.right.width);
        assert!(session.x > menu_button.right());
        assert!(session.right() <= app.right_sidebar_toggle_rect.unwrap().x);

        app.named_session_menu = Some(menu());
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert_eq!(
            app.named_session_menu_rect.unwrap().right(),
            120,
            "the right-sidebar popup is flush with the viewport edge"
        );
    }

    #[test]
    fn mobile_sheet_uses_two_row_touch_targets() {
        let _env = crate::persist::test_env("named-session-menu-mobile-render");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(64, 35, tx).unwrap();
        app.compact = true;
        app.named_session_menu = Some(menu());
        let area = Rect::new(0, 0, 64, 35);
        let mut buffer = Buffer::empty(area);
        let mut target = crate::ui::RenderTarget::new(&mut buffer, area);
        let theme = app.theme.clone();
        draw_session_menu(&mut target, area, &mut app, &theme);

        assert_eq!(app.named_session_menu_rect, Some(area));
        assert_eq!(app.named_session_row_rects.len(), 4);
        assert!(app
            .named_session_row_rects
            .iter()
            .all(|(_, rect)| rect.height == 2));
        let close = app.named_session_close_rect.unwrap();
        assert_eq!(close.height, 2);
        assert!(app
            .named_session_row_rects
            .iter()
            .all(|(_, rect)| rect.y >= close.bottom()));
    }
}
