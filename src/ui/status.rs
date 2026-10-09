//! The bottom status line. Fixed guidance owns the left edge, Luvus Bar owns
//! the right edge, leaving the middle for the quiet notification preview.

use super::*;
use crate::app::SidebarListFocus;

/// A projected owner may expose extension widgets without duplicating local
/// status guidance or version chrome. Actions still run in the owning App.
pub(super) fn draw_owner_bar(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    if area.height == 0 {
        return;
    }
    let (hits, overflow) = {
        let mut candidates =
            app.bar
                .widgets_for(crate::bar::BarRegion::BottomRight, &app.config.bars, false);
        candidates.retain(|candidate| candidate.widget.key.owner != "core");
        let layout = crate::bar::compose(
            &candidates,
            area.width.min(crate::bar::MAX_BAR_REGION_WIDTH),
            crate::bar::MAX_BAR_WIDGET_WIDTH,
        );
        crate::bar::render::draw_region(
            f,
            area,
            crate::bar::BarRegion::BottomRight,
            &candidates,
            &layout,
            t,
        )
    };
    app.bar.hits.extend(hits);
    if let Some(overflow) = overflow {
        app.bar.overflow_hits.push(overflow);
    }
}

pub(super) fn draw_status(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    if area.height == 0 {
        return;
    }
    f.render_widget(Block::new().style(Style::new().bg(t.crust)), area);
    app.version_rect = None;

    let inbox = app.notifications_in_inbox() && !app.bar.history.is_empty();
    let guidance_budget = if inbox && app.mode == Mode::Normal {
        area.width.saturating_sub((area.width / 2).min(40))
    } else {
        area.width
    };
    let (left, show_bar) = fixed_guidance(app, t, guidance_budget);
    let left_width = (left.width() as u16).min(area.width);
    let mut x = area.x;
    let caps: Vec<_> = left
        .spans
        .iter()
        .map(|span| {
            let rect = Rect::new(x, area.y, span.width() as u16, 1);
            x = rect.right();
            (rect, span.style.bg)
        })
        .collect();
    f.render_widget(Paragraph::new(left), area);
    for (rect, fill) in caps {
        if let Some(fill) = fill.filter(|_| rect.right() <= area.right()) {
            f.pill_caps(rect, fill, t.crust);
        }
    }
    if !show_bar {
        return;
    }
    let available = area.width.saturating_sub(left_width + 1);
    // Reserve useful preview space before composing optional status widgets.
    // The remaining cells go back to the preview when the widgets are short.
    let preview_min = if inbox { available.min(28) } else { 0 };
    let (hits, overflow, bar_width) = {
        let candidates =
            app.bar
                .widgets_for(crate::bar::BarRegion::BottomRight, &app.config.bars, false);
        let budget = available
            .saturating_sub(preview_min)
            .min(crate::bar::MAX_BAR_REGION_WIDTH);
        let layout = crate::bar::compose(&candidates, budget, crate::bar::MAX_BAR_WIDGET_WIDTH);
        let (hits, overflow) = crate::bar::render::draw_region(
            f,
            Rect::new(area.right().saturating_sub(budget + 1), area.y, budget, 1),
            crate::bar::BarRegion::BottomRight,
            &candidates,
            &layout,
            t,
        );
        (hits, overflow, layout.width)
    };
    app.bar.hits.extend(hits);
    if let Some(overflow) = overflow {
        app.bar.overflow_hits.push(overflow);
    }
    if inbox {
        super::notifications::preview(
            f,
            Rect::new(
                area.x + left_width,
                area.y,
                available.saturating_sub(bar_width + 1),
                1,
            ),
            app,
            t,
        );
    }
}

fn fixed_guidance(app: &App, t: &Theme, budget: u16) -> (Line<'static>, bool) {
    let cat = app.catalog;
    let mut left = vec![Span::raw(" ")];
    if let Some(search) = app.pane_search.as_ref() {
        return (local_search_guidance(&search.local, cat, t), false);
    }
    if app.scroll_pane.is_some() {
        left.push(mode_label(cat.mode_scroll, t));
        left.push(Span::raw("  "));
        left.extend(hint("1-9", cat.scroll_jump, t));
        left.extend(hint("j/k f/b ↑↓", cat.act_scroll, t));
        left.extend(hint("g/G", cat.scroll_ends, t));
        left.extend(hint("/", cat.act_search, t));
        left.extend(hint("q", cat.scroll_live, t));
        return (Line::from(left), false);
    }
    if let Some(copy) = app.copy_mode {
        // Vim's showcmd: a typed count is invisible otherwise, so `12j` looks
        // like a dead keypress until the motion lands.
        let count = (copy.pending_count > 0).then(|| copy.pending_count.to_string());
        // The row is clipped, never wrapped, and a translated hint set plus a
        // pending count outgrows 80 columns in most catalogs. The two hints that
        // have to survive that are how you leave with the selection and how you
        // leave without it, so these give way instead, last one first. Arrows are
        // guessable in a selection mode and the anchor is a refinement; being
        // unable to find `q` is not recoverable by guessing.
        let optional = [
            ("/", cat.act_search),
            ("hjkl arrows", cat.act_move),
            ("v", cat.copy_anchor),
        ];
        let mut keep = optional.len();
        loop {
            let line = copy_guidance(cat, t, count.as_deref(), &optional[..keep]);
            if keep == 0 || line.width() <= usize::from(budget) {
                return (line, false);
            }
            keep -= 1;
        }
    }
    if let Some(menu) = app.named_session_menu.as_ref() {
        let label = cat.named_sessions.to_uppercase();
        left.push(mode_label(&label, t));
        left.push(Span::raw("  "));
        if menu.prompt.is_some() {
            left.extend(hint("Enter", cat.act_create, t));
        } else {
            left.extend(hint("j/k", cat.act_move, t));
            left.extend(hint(
                "Enter",
                if app.session_menu.is_some() {
                    cat.act_select
                } else {
                    cat.act_open_menu
                },
                t,
            ));
            if app.session_menu.is_none() {
                left.extend(hint("a", cat.act_right_click, t));
            }
        }
        left.extend(hint("Esc", cat.act_back, t));
        return (Line::from(left), false);
    }
    if let Some(focus) = app.sidebar_focus {
        let agents = focus == SidebarListFocus::Agents;
        left.push(mode_label(
            if agents {
                app.catalog.agents
            } else {
                app.catalog.workspaces
            },
            t,
        ));
        left.push(Span::raw("  "));
        left.extend(hint("j/k", cat.act_move, t));
        left.extend(hint("Enter", cat.act_open_menu, t));
        left.extend(hint("a", cat.act_right_click, t));
        if agents {
            left.extend(hint("f", cat.act_filter, t));
        }
        left.extend(hint("Esc", cat.act_back, t));
        return (Line::from(left), false);
    }
    if app.files_focused {
        let diff = app.files_mode == crate::diff::FilesMode::Diff;
        left.push(mode_label(if diff { "DIFF" } else { "FILES" }, t));
        left.push(Span::raw("  "));
        if !diff && app.file_tree.filter.is_some() {
            left.extend(hint("↑/↓", cat.act_move, t));
            left.extend(hint("Enter", cat.act_right_click, t));
            left.extend(hint("Esc", cat.act_back, t));
            return (Line::from(left), false);
        }
        left.extend(hint(if diff { "j/k" } else { "hjkl" }, cat.act_move, t));
        left.extend(hint("Enter", cat.act_open_menu, t));
        left.extend(hint("a", cat.act_right_click, t));
        left.extend(hint("f", cat.act_filter, t));
        left.extend(hint("Esc", cat.act_back, t));
        return (Line::from(left), false);
    }
    let focused_view = app
        .workspaces
        .get(app.active_ws)
        .and_then(|workspace| workspace.tabs.get(workspace.active_tab))
        .and_then(|tab| app.views.get(&tab.layout.focus));
    if app.mode == Mode::Normal {
        let native_search = match focused_view {
            Some(crate::app::ViewKind::File(view)) => view.search.as_ref(),
            Some(
                crate::app::ViewKind::Preview(_)
                | crate::app::ViewKind::Diff(_)
                | crate::app::ViewKind::Remote(_),
            )
            | None => None,
        };
        if let Some(search) = native_search {
            return (local_search_guidance(search, cat, t), false);
        }
        if matches!(focused_view, Some(crate::app::ViewKind::File(_))) {
            left.push(mode_label("FILE", t));
            left.push(Span::raw("  "));
            left.extend(hint("j/k", cat.act_scroll, t));
            left.extend(hint("/", cat.act_search, t));
            left.extend(hint("y", cat.act_copy, t));
            left.extend(hint("x", cat.act_close, t));
            return (Line::from(left), false);
        }
    }
    if app.mode == Mode::Resize {
        left.push(mode_label(cat.mode_resize, t));
        left.push(Span::styled(
            format!("  {}", cat.mode_resize_hint),
            Style::new().fg(t.subtext0),
        ));
        return (Line::from(left), false);
    }
    if app.mode == Mode::PaneNavigate {
        let label = match app.pane_navigation.map(|navigation| navigation.candidate) {
            Some(PaneNavigationTarget::Pane(pane)) => format!("{} p{}", cat.pane, pane.0),
            Some(PaneNavigationTarget::Commander) => cat.commander_title.to_string(),
            None => cat.pane.to_string(),
        };
        left.push(mode_label(&label, t));
        left.push(Span::raw("  "));
        left.extend(hint("←↓↑→", cat.act_move, t));
        left.extend(hint("Enter", cat.act_select, t));
        left.extend(hint("Esc", cat.act_back, t));
        return (Line::from(left), false);
    }

    let key = |command: crate::app::Cmd| {
        if let Some(view) = app.remote_workspace_view(app.active_ws) {
            if !crate::app::remote::outer_command(command) {
                return view
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.prefix_bindings.as_ref())
                    .and_then(|bindings| bindings.get(command.id()))
                    .cloned()
                    .unwrap_or_default();
            }
        }
        app.key_for(command)
    };
    if app.mode == Mode::Prefix {
        left.push(mode_label(
            &format!("{} {}", app.prefix.label(), cat.mode_prefix),
            t,
        ));
        left.push(Span::raw("  "));
        left.extend(hint("?", cat.all_shortcuts, t));
        left.extend(hint("←↓↑→", cat.pane, t));
        left.extend(compound_hint(
            &[
                key(crate::app::Cmd::SplitRight),
                key(crate::app::Cmd::SplitDown),
            ],
            cat.act_split,
            t,
        ));
        left.extend(hint(&key(crate::app::Cmd::ClosePane), cat.act_close, t));
        left.extend(hint(&key(crate::app::Cmd::NewTab), cat.act_new_tab, t));
        left.extend(compound_hint(
            &[key(crate::app::Cmd::NextTab), key(crate::app::Cmd::PrevTab)],
            cat.act_tab,
            t,
        ));
        left.extend(hint(&key(crate::app::Cmd::OpenMission), cat.mc_hint, t));
        left.extend(hint(&key(crate::app::Cmd::NewWorkspace), cat.workspace, t));
        left.extend(hint(&key(crate::app::Cmd::OpenGit), "git", t));
        left.extend(hint(&key(crate::app::Cmd::OpenBoard), "orch", t));
        left.extend(hint(&key(crate::app::Cmd::GlobalSearch), cat.act_search, t));
        return (Line::from(left), false);
    }

    if let Some(owner) = app
        .workspaces
        .get(app.active_ws)
        .and_then(|workspace| workspace.remote.as_ref())
    {
        left.push(mode_label(cat.remote_session, t));
        left.push(Span::raw("  "));
        left.push(Span::styled(
            truncate(
                &format!("{} / {}", owner.host, owner.session),
                usize::from(budget).saturating_sub(left.iter().map(Span::width).sum()),
            ),
            Style::new().fg(t.accent),
        ));
        return (Line::from(left), true);
    }

    (Line::from(left), true)
}

fn local_search_guidance<M>(
    search: &crate::search::local::LocalSearch<M>,
    cat: &'static crate::i18n::Catalog,
    t: &Theme,
) -> Line<'static> {
    let mut row = vec![
        Span::raw(" "),
        mode_label(&cat.act_search.to_uppercase(), t),
        Span::raw("  "),
    ];
    let query = if search.editing {
        format!("/{}▏", search.query)
    } else if search.matches.is_empty() {
        format!("/{} 0/0", search.query)
    } else {
        format!(
            "/{} {}/{}{}",
            search.query,
            search.current + 1,
            search.matches.len(),
            if search.truncated { "+" } else { "" }
        )
    };
    row.push(Span::styled(query, Style::new().fg(t.text).bold()));
    if search.case_sensitive {
        row.push(Span::styled("  Aa", Style::new().fg(t.overlay1)));
    }
    row.push(Span::raw("  "));
    row.extend(hint("Ctrl-I", cat.act_case, t));
    row.extend(hint("Ctrl-U", cat.act_clear, t));
    if search.editing {
        row.extend(hint("Enter", cat.act_select, t));
    } else if !search.matches.is_empty() {
        row.extend(hint("n/N", cat.act_move, t));
    }
    row.extend(hint("Esc", cat.act_cancel, t));
    Line::from(row)
}

fn mode_label(label: &str, t: &Theme) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::new().fg(t.on_color(t.accent)).bg(t.accent).bold(),
    )
}

/// Render one actionable status hint, omitting commands the user explicitly unbound.
fn hint(key: &str, word: &str, t: &Theme) -> Vec<Span<'static>> {
    if key.is_empty() {
        return Vec::new();
    }

    vec![
        Span::styled(key.to_string(), Style::new().fg(t.accent).bold()),
        Span::styled(format!(" {word}   "), Style::new().fg(t.subtext0)),
    ]
}

/// Copy mode's guidance row carrying `optional` hints. The mode label, the copy
/// key and the cancel key are always present; the caller trims `optional` down
/// until the row fits the width it actually has.
fn copy_guidance(
    cat: &'static crate::i18n::Catalog,
    t: &Theme,
    count: Option<&str>,
    optional: &[(&str, &str)],
) -> Line<'static> {
    let mut row = vec![
        Span::raw(" "),
        mode_label(cat.mode_copy, t),
        Span::raw("  "),
    ];
    if let Some(count) = count {
        row.extend(hint(count, cat.copy_count, t));
    }
    for (key, word) in optional {
        row.extend(hint(key, word, t));
    }
    row.extend(hint("y", cat.act_copy, t));
    row.extend(hint("q", cat.act_cancel, t));
    Line::from(row)
}

/// Render a slash-separated hint only when every constituent command is bound.
fn compound_hint(keys: &[String], word: &str, t: &Theme) -> Vec<Span<'static>> {
    if keys.iter().any(String::is_empty) {
        return Vec::new();
    }

    hint(&keys.join("/"), word, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn row(term: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = term.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect()
    }

    #[test]
    fn remote_status_identifies_owner_and_prefix_without_leaking_to_local_workspace() {
        let _env = crate::persist::test_env("remote-status-owner");
        let mut app = crate::app::remote::tests::remote_ui_app();
        let (_, _receiver, _) = crate::app::remote::tests::add_remote_workspace(&mut app);
        assert!(app.set_prefix("ctrl+b"));
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        for mode in [Mode::Normal, Mode::Prefix] {
            app.mode = mode;
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let status = row(&terminal, 29);
            assert_eq!(
                status.contains("dev-207 / api"),
                mode == Mode::Normal,
                "{status}"
            );
            assert_eq!(status.contains('?'), mode == Mode::Prefix, "{status}");
            assert_eq!(
                status.contains(&app.prefix.label()),
                mode == Mode::Prefix,
                "{status}"
            );
        }
        app.active_ws = 0;
        app.mode = Mode::Normal;
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert!(!row(&terminal, 29).contains("dev-207"));
    }

    #[test]
    fn normal_status_is_compact_and_shortcuts_only_appear_after_prefix() {
        let _env = crate::persist::test_env("bar-status-default");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 30, tx).unwrap();
        app.config.layout.rounded_corners = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let status = row(&terminal, 29);
        assert!(!status.contains(&app.prefix.label()));
        assert!(!status.contains(env!("CARGO_PKG_VERSION")));
        assert!(app.version_rect.is_none());
        assert!(status.trim().len() < 35, "{status}");
        app.mode = Mode::Prefix;
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let status = row(&terminal, 29);
        assert!(status.contains(&app.prefix.label()), "{status}");
        assert!(status.contains('?'));
        assert!(status.contains('◖') && status.contains('◗'), "{status}");
    }

    #[test]
    fn external_bottom_widgets_yield_to_prefix_guidance() {
        let _env = crate::persist::test_env("bar-status-fixed-lanes");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let mut segment =
            crate::bar::BarSegment::text("deploy ready", crate::bar::BarTone::Success);
        segment.action = Some("details".into());
        let widget = crate::bar::BarWidget::new(
            crate::bar::BarWidgetKey::new("example", "deploy"),
            crate::bar::BarRegion::BottomRight,
            vec![segment],
            Vec::new(),
            50,
        )
        .unwrap();
        app.bar.push_widget(widget).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert!(app.bar.hits.iter().any(|hit| hit.key.owner == "example"));
        assert!(app.bar.hits.iter().all(|hit| hit.rect.right() < 80));

        app.mode = Mode::Prefix;
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert!(app.version_rect.is_none());
        assert!(
            app.bar.hits.is_empty(),
            "mode guidance temporarily owns the middle lane"
        );
    }

    #[test]
    fn prefix_guidance_shows_the_mission_control_binding() {
        let _env = crate::persist::test_env("bar-status-mission");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        app.mode = Mode::Prefix;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        let status = row(&terminal, 23);
        assert_eq!(app.key_for(crate::app::Cmd::OpenMission), "m");
        let mission = format!(
            "{} {}",
            app.key_for(crate::app::Cmd::OpenMission),
            app.catalog.mc_hint
        );
        assert!(
            status.contains(&mission),
            "prefix guidance omitted Mission Control: {status:?}"
        );
    }

    #[test]
    fn prefix_guidance_omits_unbound_mission_control() {
        let _env = crate::persist::test_env("bar-status-mission-unbound");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        app.config
            .keybindings
            .insert(crate::app::Cmd::OpenMission.id().to_string(), String::new());
        app.mode = Mode::Prefix;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        assert!(app.key_for(crate::app::Cmd::OpenMission).is_empty());
        let status = row(&terminal, 23);
        assert!(
            !status.contains(app.catalog.mc_hint),
            "prefix guidance advertised unbound Mission Control: {status:?}"
        );
    }

    #[test]
    fn prefix_guidance_omits_incomplete_compound_bindings() {
        let _env = crate::persist::test_env("bar-status-compound-unbound");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(160, 24, tx).unwrap();
        for command in [
            crate::app::Cmd::SplitRight,
            crate::app::Cmd::NextTab,
            crate::app::Cmd::PrevTab,
        ] {
            app.config
                .keybindings
                .insert(command.id().to_string(), String::new());
        }
        app.mode = Mode::Prefix;
        let mut terminal = Terminal::new(TestBackend::new(160, 24)).unwrap();

        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        let status = row(&terminal, 23);
        assert!(
            !status.contains(app.catalog.act_split),
            "prefix guidance advertised a partially bound split: {status:?}"
        );
        assert!(
            !status.contains(&format!("/ {}", app.catalog.act_tab)),
            "prefix guidance advertised an unbound tab pair: {status:?}"
        );
        assert!(compound_hint(&[String::new(), "v".to_string()], "split", &app.theme).is_empty());
        assert!(compound_hint(&[String::new(), String::new()], "tab", &app.theme).is_empty());
    }

    #[test]
    fn files_guidance_matches_tree_and_opened_view_actions() {
        let _env = crate::persist::test_env("bar-status-files");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        app.files_focused = true;
        let theme = app.theme.clone();

        let (line, _) = fixed_guidance(&app, &theme, 120);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert!(
            text.contains("Enter open"),
            "unexpected FILES legend: {text}"
        );
        assert!(
            text.contains("a right click"),
            "unexpected FILES legend: {text}"
        );
        assert!(
            !text.contains("x close"),
            "tree legend must not advertise a file-view action: {text}"
        );

        app.files_mode = crate::diff::FilesMode::Diff;
        let (line, _) = fixed_guidance(&app, &theme, 120);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("DIFF"), "unexpected DIFF legend: {text}");
        assert!(
            text.contains("Enter open"),
            "unexpected DIFF legend: {text}"
        );
        assert!(text.contains("f filter"), "unexpected DIFF legend: {text}");

        app.files_focused = false;
        let pane = app.layout().focus;
        app.views.insert(
            pane,
            crate::app::ViewKind::File(crate::files::FileView::new("README.md".into())),
        );
        let (line, _) = fixed_guidance(&app, &theme, 120);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("x close"), "unexpected FILE legend: {text}");

        for (mode, expected) in [
            (Mode::Resize, app.catalog.mode_resize),
            (Mode::Prefix, app.catalog.mode_prefix),
        ] {
            app.mode = mode;
            let (line, _) = fixed_guidance(&app, &theme, 120);
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert!(
                text.contains(expected),
                "{mode:?} controls must override the FILE legend: {text}"
            );
            assert_eq!(
                line.spans[1].content.as_ref(),
                if mode == Mode::Prefix {
                    format!(" {} {expected} ", app.prefix.label())
                } else {
                    format!(" {expected} ")
                },
                "{mode:?} must own the leading mode label"
            );
        }
    }

    #[test]
    fn file_search_uses_outer_status_guidance() {
        let _env = crate::persist::test_env("bar-status-native-search");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        let pane = app.layout().focus;
        let theme = app.theme.clone();
        let views = vec![crate::app::ViewKind::File(crate::files::FileView::new(
            "sample.txt".into(),
        ))];

        for mut view in views {
            let search = match &mut view {
                crate::app::ViewKind::File(view) => &mut view.search,
                crate::app::ViewKind::Preview(view) => &mut view.search,
                crate::app::ViewKind::Diff(view) => &mut view.search,
                crate::app::ViewKind::Remote(_) => unreachable!("native view fixture"),
            };
            *search = Some(crate::search::local::LocalSearch {
                query: "Needle".into(),
                editing: false,
                case_sensitive: true,
                matches: vec![crate::search::local::RowMatch {
                    row: 0,
                    byte_start: 0,
                    byte_end: 6,
                    column: 0,
                    width: 6,
                }],
                current: 0,
                truncated: true,
            });
            app.views.insert(pane, view);

            let line = fixed_guidance(&app, &theme, 120).0;
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(line.spans[1].content.as_ref(), " SEARCH ");
            assert!(
                text.contains("/Needle 1/1+  Aa"),
                "native search state: {text}"
            );
            assert!(
                text.contains("n/N move"),
                "native search navigation: {text}"
            );
            assert!(text.contains("Ctrl-U clear"), "native search clear: {text}");
            assert!(text.contains("Ctrl-I case"), "native search case: {text}");
            assert!(text.contains("Esc cancel"), "native search cancel: {text}");
        }

        app.mode = Mode::Resize;
        let line = fixed_guidance(&app, &theme, 120).0;
        assert_eq!(
            line.spans[1].content.as_ref(),
            format!(" {} ", app.catalog.mode_resize),
            "focused modes keep precedence over native search"
        );
    }

    #[test]
    fn preview_and_diff_search_leave_the_outer_status_bar_available() {
        let _env = crate::persist::test_env("bar-status-diff-search-footer");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        let pane = app.layout().focus;
        let theme = app.theme.clone();
        let path = crate::diff::RepoPath::from_path(std::path::Path::new("sample.txt"))
            .expect("valid relative path");
        let key = crate::diff::DiffKey {
            repo_id: "repo".into(),
            worktree_id: "tree".into(),
            layer: crate::diff::DiffLayer::Worktree,
            old_path: Some(path.clone()),
            new_path: Some(path),
        };
        let mut view = crate::diff::DiffView::new(
            "/repo".into(),
            key,
            crate::diff::DiffLayoutPreference::Stack,
            3,
            false,
            false,
        );
        view.search = Some(crate::search::local::LocalSearch::editing());
        app.views
            .insert(pane, crate::app::ViewKind::Diff(Box::new(view)));

        let line = fixed_guidance(&app, &theme, 120).0;
        assert!(line.spans.iter().all(|span| span.content != " SEARCH "));
        assert!(
            line.spans
                .iter()
                .all(|span| !span.content.contains("Ctrl-I")),
            "DIFF search controls belong to the view footer"
        );

        let mut preview = crate::files::preview::DocumentView::new(
            "README.md".into(),
            crate::files::preview::PreviewKind::Markdown,
        );
        preview.search = Some(crate::search::local::LocalSearch::editing());
        app.views
            .insert(pane, crate::app::ViewKind::Preview(preview));
        let line = fixed_guidance(&app, &theme, 120).0;
        assert!(line.spans.iter().all(|span| span.content != " SEARCH "));
        assert!(
            line.spans
                .iter()
                .all(|span| !span.content.contains("Ctrl-I")),
            "Preview search controls belong to the view footer"
        );
    }

    #[test]
    fn scroll_and_pane_search_guidance_shows_key_hints() {
        let _env = crate::persist::test_env("bar-status-pane-search");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        let pane = app.layout().focus;
        let theme = app.theme.clone();
        let text = |app: &App| {
            fixed_guidance(app, &theme, 120)
                .0
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        app.scroll_pane = Some(pane);
        let scroll = text(&app);
        assert!(scroll.contains(app.catalog.mode_scroll));
        assert!(
            scroll.contains("/ search"),
            "scroll mode must hint /: {scroll}"
        );
        assert!(
            scroll.contains("q live"),
            "scroll mode must keep live: {scroll}"
        );

        app.pane_search = Some(crate::app::PaneSearch {
            pane,
            owner: crate::app::PaneSearchOwner::Scroll,
            local: crate::search::local::LocalSearch {
                query: "needle".into(),
                editing: true,
                case_sensitive: false,
                matches: Vec::new(),
                current: 0,
                truncated: false,
            },
            saved_scroll: 0,
        });
        let editing = text(&app);
        assert!(editing.contains("SEARCH"), "search mode label: {editing}");
        assert!(editing.contains("/needle▏"), "query caret: {editing}");
        assert!(
            editing.contains("Ctrl-I case"),
            "search must show its case toggle: {editing}"
        );
        assert!(
            !editing.contains(" Aa"),
            "insensitive mode stays quiet: {editing}"
        );
        app.pane_search.as_mut().unwrap().case_sensitive = true;
        let sensitive = text(&app);
        assert!(
            sensitive.contains("/needle▏  Aa"),
            "sensitive mode marks the query: {sensitive}"
        );
        assert!(
            editing.contains("Ctrl-U clear"),
            "editing search must hint Ctrl-U: {editing}"
        );
        assert!(
            editing.contains("Enter select"),
            "editing search must hint Enter: {editing}"
        );
        assert!(
            editing.contains("Esc cancel"),
            "editing search must hint Esc: {editing}"
        );
        assert!(
            !editing.contains("n/N"),
            "editing search must not advertise n/N: {editing}"
        );

        app.pane_search.as_mut().unwrap().editing = false;
        app.pane_search.as_mut().unwrap().matches = vec![
            crate::app::PaneSearchMatch {
                row: 0,
                col: 0,
                width: 6,
            },
            crate::app::PaneSearchMatch {
                row: 2,
                col: 0,
                width: 6,
            },
        ];
        let committed = text(&app);
        assert!(
            committed.contains("/needle 1/2"),
            "match count: {committed}"
        );
        assert!(
            committed.contains("n/N move"),
            "committed search must hint n/N: {committed}"
        );
        assert!(
            committed.contains("Ctrl-U clear"),
            "committed search must hint Ctrl-U: {committed}"
        );
        assert!(committed.contains("Esc cancel"));
        assert!(!committed.contains("Enter select"));
    }

    #[test]
    fn sidebar_guidance_matches_workspace_and_agent_actions() {
        let _env = crate::persist::test_env("bar-status-sidebar-focus");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        let theme = app.theme.clone();

        app.sidebar_focus = Some(SidebarListFocus::Workspaces);
        let (line, _) = fixed_guidance(&app, &theme, 120);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("WORKSPACES"));
        assert!(text.contains("Enter open"));
        assert!(text.contains("a right click"));

        app.sidebar_focus = Some(SidebarListFocus::Agents);
        let (line, _) = fixed_guidance(&app, &theme, 120);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("AGENTS"));
        assert!(text.contains("f filter"));
        assert!(text.contains("a right click"));
    }

    #[test]
    fn session_guidance_tracks_the_focused_selector_and_its_action_menu() {
        let _env = crate::persist::test_env("bar-status-session-focus");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 24, tx).unwrap();
        let theme = app.theme.clone();
        app.named_session_menu = Some(crate::app::session_menu::NamedSessionMenu {
            client_id: None,
            remote_targets: Vec::new(),
            hosts: Vec::new(),
            generation: 1,
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            loading: false,
            prompt: None,
            error: None,
            preparing: false,
        });

        let text = |app: &App| {
            fixed_guidance(app, &theme, 120)
                .0
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let focused = text(&app);
        assert!(focused.contains("SESSIONS"));
        assert!(focused.contains("j/k move"));
        assert!(focused.contains("Enter open"));
        assert!(focused.contains("a right click"));
        assert!(focused.contains("Esc back"));

        app.session_menu = Some(crate::app::SessionMenu {
            targets: vec![crate::app::session_menu::SessionMenuTarget {
                name: "review".into(),
                remote: None,
                running: false,
                current: false,
            }],
            confirming: None,
            anchor: (4, 4),
            items: Vec::new(),
            selected: 0,
        });
        let actions = text(&app);
        assert!(actions.contains("Enter select"));
        assert!(!actions.contains("a right click"));

        app.session_menu = None;
        app.named_session_menu.as_mut().unwrap().prompt =
            Some(crate::app::session_menu::NamedSessionPrompt::Local {
                name: String::new(),
            });
        let prompt = text(&app);
        assert!(prompt.contains("Enter create"));
        assert!(!prompt.contains("a right click"));
    }

    #[test]
    fn bottom_bar_is_right_aligned_and_capped_at_100_columns() {
        let _env = crate::persist::test_env("bar-status-100");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(200, 24, tx).unwrap();
        app.config.bars.place(crate::bar::CORE_RUNTIME, None);
        app.config.bars.place(crate::bar::CORE_FOCUSED_PANE, None);
        let mut segment =
            crate::bar::BarSegment::text("x".repeat(100), crate::bar::BarTone::Accent);
        segment.action = Some("details".into());
        let widget = crate::bar::BarWidget::new(
            crate::bar::BarWidgetKey::new("example", "wide-bottom"),
            crate::bar::BarRegion::BottomRight,
            vec![segment],
            Vec::new(),
            50,
        )
        .unwrap();
        app.bar.push_widget(widget).unwrap();

        let area = Rect::new(0, 0, 200, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        let mut target = crate::ui::RenderTarget::new(&mut buffer, area);
        let theme = app.theme.clone();
        draw_status(&mut target, area, &mut app, &theme);

        let hit = app.bar.hits.first().expect("bottom widget is visible");
        assert_eq!(hit.rect.width, crate::bar::MAX_BAR_REGION_WIDTH);
        assert_eq!(hit.rect.right(), area.right() - 1);
        assert!(app.version_rect.is_none());
    }

    #[test]
    fn focused_pane_metadata_renders_compactly_with_narrow_overflow() {
        let _env = crate::persist::test_env("bar-status-focused-pane");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(200, 24, tx).unwrap();
        let pane = app.layout().focus;
        let status = app.status.get_mut(&pane).unwrap();
        status.agent = "qodercli".into();
        status.state = crate::ui::theme::State::Working;
        app.config.bars.place(
            crate::bar::CORE_FOCUSED_PANE,
            Some(crate::bar::BarRegion::BottomRight),
        );
        app.refresh_core_bar_widgets();
        for width in [200, 120, 80, 35] {
            let area = Rect::new(0, 0, width, 1);
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            let mut target = crate::ui::RenderTarget::new(&mut buffer, area);
            let theme = app.theme.clone();
            draw_status(&mut target, area, &mut app, &theme);
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(
                !text.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
                "{width}: {text}"
            );
            if width == 200 {
                assert!(text.contains(&format!("p{}", pane.0)), "{text}");
                assert!(text.contains("qodercli working"), "{text}");
            }
        }
    }
    /// Copy mode's guidance is clipped, never wrapped, so a row wider than the
    /// available status row loses its tail silently. Cancel and copy are
    /// how you leave the mode with or without the selection, so they have to
    /// survive every catalog at the widest count the mode can hold. English alone
    /// proves nothing here: it is the shortest of the eight, and the row only fits
    /// it by a couple of columns.
    #[test]
    fn copy_mode_guidance_keeps_copy_and_cancel_in_every_language() {
        let _env = crate::persist::test_env("bar-status-copy-width");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus;
        app.copy_mode = Some(crate::app::CopyMode {
            pane,
            anchor: (0, 0),
            cursor: (0, 0),
            saved_scroll: 0,
            pending_count: crate::app::COPY_COUNT_MAX,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        // The real budget, taken from the rendered layout rather than restated.
        let budget = terminal.backend().buffer().area.width;
        let t = app.theme.clone();

        for code in crate::i18n::LANGS {
            app.catalog = crate::i18n::by_code(code);
            let cat = app.catalog;
            let (line, _) = fixed_guidance(&app, &t, budget);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                line.width() <= usize::from(budget),
                "{code} guidance is {} columns for a {budget}-column row:\n{text}",
                line.width()
            );
            assert!(
                text.contains(cat.act_cancel),
                "{code} must still show how to leave:\n{text}"
            );
            assert!(
                text.contains(cat.act_copy),
                "{code} must still show how to copy:\n{text}"
            );
        }

        // Trimming is a last resort, not the normal path: with room to spare the
        // row still carries everything. Without this a always-drop bug would pass.
        app.catalog = crate::i18n::by_code("en");
        if let Some(copy) = app.copy_mode.as_mut() {
            copy.pending_count = 0;
        }
        let (line, _) = fixed_guidance(&app, &t, budget);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("/ search")
                && text.contains(app.catalog.act_move)
                && text.contains(app.catalog.copy_anchor),
            "an uncrowded row keeps its optional hints:\n{text}"
        );
    }
}
