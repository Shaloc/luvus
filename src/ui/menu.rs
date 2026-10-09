//! Right-click context menus (workspace + pane), drawn as a small popup anchored
//! at the click point.

use super::*;
use crate::app::{
    AgentMenuItem, DiffMenuItem, FileMenuItem, MenuScroll, ModuleMenuAction, OrchMenuItem,
    PaneMenuItem, PopupId, TabMenuItem, WsMenuItem,
};
use crate::i18n::Catalog;
use ratatui::widgets::{Borders, Clear};

/// One row of a context-menu popup.
struct MenuRow {
    text: String,
    divider: bool,
    destructive: bool,
}

fn row_is_hovered(row: Rect, hover: Option<(u16, u16)>) -> bool {
    hover.is_some_and(|(column, pointer_row)| {
        column >= row.x
            && column < row.right()
            && pointer_row >= row.y
            && pointer_row < row.bottom()
    })
}

/// What a popup needs from the app to draw itself: where the cursor is, whether
/// this is the compact layout, and which popup it is — the last so its scroll
/// offset survives between frames without every caller tracking one.
struct PopupCtx<'a> {
    hover: Option<(u16, u16)>,
    selected: Option<usize>,
    mobile: bool,
    id: PopupId,
    scroll: &'a mut MenuScroll,
}

/// Render a context-menu popup anchored near `anchor` (clamped so it stays on
/// screen) and return one clickable rect per row — dividers included — in order,
/// for the input layer to hit-test.
///
/// A popup taller than the space it has scrolls, and a row that is out of view
/// gets an empty rect: it can never be hit, and callers keep zipping their items
/// against the full-length result. Dropping the overflow instead — which is what
/// this did before — made the rows a menu puts last unreachable rather than
/// merely unpainted.
fn render_popup(
    f: &mut RenderTarget,
    area: Rect,
    anchor: (u16, u16),
    rows: &[MenuRow],
    t: &Theme,
    ctx: PopupCtx<'_>,
) -> Vec<Rect> {
    let PopupCtx {
        hover,
        selected,
        mobile,
        id,
        scroll,
    } = ctx;
    let (ax, ay) = anchor;
    // Size the box to the widest label (+ a leading pad + the border).
    let label_w = rows
        .iter()
        .map(|r| super::display_width(&r.text))
        .max()
        .unwrap_or(6) as u16;
    let row_height = if mobile { 2 } else { 1 };
    let w = if mobile {
        area.width.max(1)
    } else {
        (label_w + 4).max(12).min(area.width.max(1))
    };
    let h = ((rows.len() as u16).saturating_mul(row_height) + 2).min(area.height.max(1));
    let x = if mobile {
        area.x
    } else {
        ax.min(area.right().saturating_sub(w)).max(area.x)
    };
    let y = if mobile {
        area.bottom().saturating_sub(h)
    } else {
        ay.min(area.bottom().saturating_sub(h)).max(area.y)
    };
    let popup = Rect::new(x, y, w, h);

    // Rounded outlines need the surrounding surface in their outer corner
    // cells; filling those cells with the menu color makes a square silhouette.
    let corners = [
        (popup.x, popup.y),
        (popup.right() - 1, popup.y),
        (popup.x, popup.bottom() - 1),
        (popup.right() - 1, popup.bottom() - 1),
    ]
    .map(|at| (at, f.buffer_mut().cell(at).map(|cell| cell.bg)));
    f.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(f.border_type())
        .border_style(
            Style::new()
                .fg(if f.rounded_corners {
                    t.overlay0
                } else {
                    t.border_focus
                })
                .bg(t.surface0),
        )
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if f.rounded_corners {
        for (at, bg) in corners {
            if let (Some(cell), Some(bg)) = (f.buffer_mut().cell_mut(at), bg) {
                cell.set_bg(bg);
            }
        }
    }

    // How many rows fit, and how far the list can therefore be scrolled.
    let per_screen = (inner.height / row_height) as usize;
    let max_offset = rows.len().saturating_sub(per_screen);
    if let Some(selected) = selected {
        scroll.reveal(id, selected, per_screen, max_offset);
    }
    let offset = scroll.record(id, popup, max_offset);

    let mut rects = vec![Rect::default(); rows.len()];
    for (slot, i) in (offset..rows.len()).take(per_screen).enumerate() {
        let r = &rows[i];
        let slot = slot as u16;
        let row = Rect::new(
            inner.x,
            inner.y + slot * row_height,
            inner.width,
            row_height.min(inner.bottom().saturating_sub(inner.y + slot * row_height)),
        );
        if row.height == 0 {
            break;
        }
        let text_row = Rect::new(
            row.x,
            row.y + row.height.saturating_sub(1) / 2,
            row.width,
            1,
        );
        if r.divider {
            // A thin, non-interactive separator across the inner width.
            let line = "─".repeat(inner.width as usize);
            f.render_widget(
                Paragraph::new(Span::styled(
                    line,
                    Style::new().fg(t.surface1).bg(t.surface0),
                )),
                text_row,
            );
            rects[i] = row;
            continue;
        }
        let hot = selected.map_or_else(|| row_is_hovered(row, hover), |index| index == i);
        let fg = if hot {
            t.on_color(t.accent)
        } else if r.destructive {
            t.coral // the one destructive action
        } else {
            t.text
        };
        let bg = if hot { t.accent } else { t.surface0 };
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {} ", r.text),
                Style::new().fg(fg).bg(bg),
            )),
            text_row,
        );
        if hot {
            f.pill_caps(text_row, bg, t.surface0);
        }
        rects[i] = row;
    }

    // Say when rows are out of view, so a clipped menu does not read as a whole
    // one. The marker sits in the border, which costs no row.
    if popup.width >= 3 {
        let marker_x = popup.right().saturating_sub(2);
        let style = Style::new().fg(t.accent).bg(t.surface0);
        if offset > 0 {
            f.render_widget(
                Paragraph::new(Span::styled("\u{25b2}", style)),
                Rect::new(marker_x, popup.y, 1, 1),
            );
        }
        if offset < max_offset {
            f.render_widget(
                Paragraph::new(Span::styled("\u{25bc}", style)),
                Rect::new(marker_x, popup.bottom().saturating_sub(1), 1, 1),
            );
        }
    }
    rects
}

pub(super) fn draw_ws_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(index) = app.ws_menu_target_index() else {
        app.ws_menu = None;
        return;
    };
    let Some(menu) = app.ws_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let selected = menu.selected;
    let quick_selected = menu.quick_selected;
    let extras = menu.module_actions.clone();
    // Submenu rects from the *previous* frame, so hovering the submenu itself
    // keeps it open (this frame's rects are not computed yet).
    let previous_quick_rects = menu.quick_rects.clone();
    let quick = app.ws_quick_actions(index);
    let items = app.ws_menu_items(index);
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: {
                let label = ws_label(*it, cat, &extras, app.config.layout.workspace_paths);
                if matches!(it, WsMenuItem::OwnerSettings | WsMenuItem::OwnerModules) {
                    app.workspaces[index]
                        .remote
                        .as_ref()
                        .map_or(label.clone(), |owner| format!("{label} [{}]", owner.host))
                } else {
                    label
                }
            },
            divider: matches!(it, WsMenuItem::Divider),
            destructive: matches!(it, WsMenuItem::Close | WsMenuItem::DeleteWorktree),
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected,
            mobile: app.compact,
            id: PopupId::Ws,
            scroll: &mut app.menu_scroll,
        },
    );
    let quick_rect = items
        .iter()
        .zip(&rects)
        .find(|(it, _)| **it == WsMenuItem::QuickActions)
        .map(|(_, rect)| *rect)
        // A parent row that scrolled out of view has no rect to hang a submenu
        // off, and an empty one would anchor it at the origin.
        .filter(|rect: &Rect| rect.height > 0);
    if let Some(menu) = app.ws_menu.as_mut() {
        menu.items = items.iter().copied().zip(rects.iter().copied()).collect();
    }

    // Sticky open/close of the submenu based on where the cursor is: over the
    // Quick Actions row or the submenu opens it; over another main row closes it;
    // over the border gap between them leaves it unchanged (so it doesn't flicker).
    if let (Some(parent), Some(hover)) = (quick_rect, app.hover) {
        let in_rect = |rect: &Rect| {
            hover.0 >= rect.x
                && hover.0 < rect.right()
                && hover.1 >= rect.y
                && hover.1 < rect.bottom()
        };
        let over_parent = in_rect(&parent);
        let over_submenu = previous_quick_rects.iter().any(|(_, rect)| in_rect(rect));
        let over_other = items.iter().zip(&rects).any(|(item, rect)| {
            !matches!(item, WsMenuItem::QuickActions | WsMenuItem::Divider) && in_rect(rect)
        });
        if let Some(menu) = app.ws_menu.as_mut() {
            if over_parent || over_submenu {
                menu.quick_open = true;
            } else if over_other {
                menu.quick_open = false;
                menu.quick_selected = None;
            }
        }
    }

    let open = app.ws_menu.as_ref().is_some_and(|menu| menu.quick_open);
    if let (Some(parent), false) = (open.then_some(()).and(quick_rect), quick.is_empty()) {
        let sub_rows: Vec<MenuRow> = quick
            .iter()
            .map(|it| MenuRow {
                text: ws_label(*it, cat, &extras, app.config.layout.workspace_paths),
                divider: false,
                destructive: false,
            })
            .collect();
        // Beside the main popup, first row aligned with the Quick Actions row.
        let sub_anchor = (parent.right() + 1, parent.y.saturating_sub(1));
        let sub_rects = render_popup(
            f,
            area,
            sub_anchor,
            &sub_rows,
            t,
            PopupCtx {
                hover: app.hover,
                selected: quick_selected,
                mobile: app.compact,
                id: PopupId::WsQuick,
                scroll: &mut app.menu_scroll,
            },
        );
        if let Some(menu) = app.ws_menu.as_mut() {
            menu.quick_rects = quick.iter().copied().zip(sub_rects).collect();
        }
    } else if let Some(menu) = app.ws_menu.as_mut() {
        menu.quick_rects.clear();
        menu.quick_selected = None;
    }
}

pub(super) fn draw_tab_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.tab_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let extras = menu.module_actions.clone();
    let swap_targets = menu.swap_targets.clone();
    let previous_swap_rects = menu.swap_rects.clone();

    let items = app.tab_menu_items();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|item| MenuRow {
            text: tab_label(*item, cat, &extras),
            divider: matches!(item, TabMenuItem::Divider),
            destructive: false,
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected: None,
            mobile: app.compact,
            id: PopupId::Tab,
            scroll: &mut app.menu_scroll,
        },
    );
    let swap_rect = items
        .iter()
        .zip(&rects)
        .find(|(item, _)| **item == TabMenuItem::SwapWith)
        .map(|(_, rect)| *rect)
        // A parent row that scrolled out of view has no rect to hang a submenu
        // off, and an empty one would anchor it at the origin.
        .filter(|rect: &Rect| rect.height > 0);
    if let Some(menu) = app.tab_menu.as_mut() {
        menu.items = items.iter().copied().zip(rects.iter().copied()).collect();
    }

    // Keep the submenu open across the one-column gap between both popups.
    if let (Some(parent), Some(hover)) = (swap_rect, app.hover) {
        let in_rect = |rect: &Rect| {
            hover.0 >= rect.x
                && hover.0 < rect.right()
                && hover.1 >= rect.y
                && hover.1 < rect.bottom()
        };
        let over_parent = in_rect(&parent);
        let over_submenu = previous_swap_rects.iter().any(|(_, rect)| in_rect(rect));
        let over_other = items.iter().zip(&rects).any(|(item, rect)| {
            !matches!(item, TabMenuItem::SwapWith | TabMenuItem::Divider) && in_rect(rect)
        });
        if let Some(menu) = app.tab_menu.as_mut() {
            if over_parent || over_submenu {
                menu.swap_open = true;
            } else if over_other {
                menu.swap_open = false;
            }
        }
    }

    let open = app.tab_menu.as_ref().is_some_and(|menu| menu.swap_open);
    if let (Some(parent), false) = (open.then_some(()).and(swap_rect), swap_targets.is_empty()) {
        let sub_rows: Vec<MenuRow> = swap_targets
            .iter()
            .map(|(_, label)| MenuRow {
                text: label.clone(),
                divider: false,
                destructive: false,
            })
            .collect();
        let sub_anchor = (parent.right() + 1, parent.y.saturating_sub(1));
        let sub_rects = render_popup(
            f,
            area,
            sub_anchor,
            &sub_rows,
            t,
            PopupCtx {
                hover: app.hover,
                selected: None,
                mobile: app.compact,
                id: PopupId::TabSwap,
                scroll: &mut app.menu_scroll,
            },
        );
        if let Some(menu) = app.tab_menu.as_mut() {
            menu.swap_rects = swap_targets
                .iter()
                .map(|(target, _)| target.clone())
                .zip(sub_rects)
                .collect();
        }
    } else if let Some(menu) = app.tab_menu.as_mut() {
        menu.swap_rects.clear();
    }
}

pub(super) fn draw_pane_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.pane_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let extras = menu.module_actions.clone();
    let move_targets = menu.move_targets.clone();
    // Submenu rects from the *previous* frame, to keep the submenu open while the
    // cursor is over it (before we recompute this frame's rects).
    let prev_tab_rects = menu.tab_rects.clone();

    let items = app.pane_menu_items();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: pane_label(*it, cat, &extras),
            divider: matches!(it, PaneMenuItem::Divider),
            destructive: matches!(it, PaneMenuItem::Close),
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected: None,
            mobile: app.compact,
            id: PopupId::Pane,
            scroll: &mut app.menu_scroll,
        },
    );
    let move_rect = items
        .iter()
        .zip(&rects)
        .find(|(it, _)| **it == PaneMenuItem::MoveToTab)
        .map(|(_, r)| *r)
        // A parent row that scrolled out of view has no rect to hang a submenu
        // off, and an empty one would anchor it at the origin.
        .filter(|rect: &Rect| rect.height > 0);
    if let Some(menu) = app.pane_menu.as_mut() {
        menu.items = items.iter().copied().zip(rects.iter().copied()).collect();
    }

    // Sticky open/close of the submenu based on where the cursor is: over the
    // "Move to tab" row or the submenu opens it; over another main row closes it;
    // over the border gap between them leaves it unchanged (so it doesn't flicker).
    if let (Some(mrect), Some(hov)) = (move_rect, app.hover) {
        let in_r =
            |r: &Rect| hov.0 >= r.x && hov.0 < r.right() && hov.1 >= r.y && hov.1 < r.bottom();
        let over_move = in_r(&mrect);
        let over_submenu = prev_tab_rects.iter().any(|(_, r)| in_r(r));
        let over_other = items.iter().zip(&rects).any(|(it, r)| {
            !matches!(it, PaneMenuItem::MoveToTab | PaneMenuItem::Divider) && in_r(r)
        });
        if let Some(menu) = app.pane_menu.as_mut() {
            if over_move || over_submenu {
                menu.move_open = true;
            } else if over_other {
                menu.move_open = false;
            }
        }
    }

    let open = app.pane_menu.as_ref().is_some_and(|m| m.move_open);
    match (open.then_some(()).and(move_rect), move_targets.is_empty()) {
        (Some(mrect), false) => {
            let sub_rows: Vec<MenuRow> = move_targets
                .iter()
                .map(|(_, label)| MenuRow {
                    text: label.clone(),
                    divider: false,
                    destructive: false,
                })
                .collect();
            // Beside the main popup, first row aligned with the "Move to tab" row.
            let sub_anchor = (mrect.right() + 1, mrect.y.saturating_sub(1));
            let sub_rects = render_popup(
                f,
                area,
                sub_anchor,
                &sub_rows,
                t,
                PopupCtx {
                    hover: app.hover,
                    selected: None,
                    mobile: app.compact,
                    id: PopupId::PaneMove,
                    scroll: &mut app.menu_scroll,
                },
            );
            if let Some(menu) = app.pane_menu.as_mut() {
                menu.tab_rects = move_targets
                    .iter()
                    .map(|(tg, _)| *tg)
                    .zip(sub_rects)
                    .collect();
            }
        }
        _ => {
            if let Some(menu) = app.pane_menu.as_mut() {
                menu.tab_rects.clear();
            }
        }
    }
}

pub(super) fn draw_agent_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.agent_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let selected = menu.selected;
    let items = app.agent_menu_items(menu.target.clone());
    let extras = menu.module_actions.clone();
    let scoped = app.agents_this_workspace;
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: {
                let label = agent_label(*it, cat, &extras, scoped, app.config.layout.agent_paths);
                let checked = match it {
                    AgentMenuItem::ToggleStatusNames => Some(app.config.layout.agent_status_names),
                    AgentMenuItem::ToggleWorkspaceGrouping => {
                        Some(app.config.agents_group_by_workspace)
                    }
                    AgentMenuItem::ToggleStatus(state) => {
                        Some(app.config.agents_status_filter.includes(*state))
                    }
                    AgentMenuItem::AllStatuses => {
                        Some(app.config.agents_status_filter == Default::default())
                    }
                    _ => None,
                };
                checked.map_or(label.clone(), |checked| {
                    format!("[{}] {label}", if checked { "x" } else { " " })
                })
            },
            divider: matches!(it, AgentMenuItem::Divider),
            destructive: matches!(it, AgentMenuItem::Close | AgentMenuItem::AutomationDelete),
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected,
            mobile: app.compact,
            id: PopupId::Agent,
            scroll: &mut app.menu_scroll,
        },
    );
    if let Some(menu) = app.agent_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

fn agent_label(
    it: AgentMenuItem,
    cat: &Catalog,
    extras: &[ModuleMenuAction],
    scoped: bool,
    paths_visible: bool,
) -> String {
    match it {
        AgentMenuItem::ToggleWorkspaceGrouping => cat.menu_group_workspaces.into(),
        AgentMenuItem::ToggleStatusNames => cat.menu_status_names.into(),
        AgentMenuItem::CollapseWorkspace => cat.menu_collapse_workspace.into(),
        AgentMenuItem::ExpandWorkspace => cat.menu_expand_workspace.into(),
        AgentMenuItem::AllStatuses => cat.menu_all_statuses.into(),
        AgentMenuItem::ToggleStatus(state) => {
            use crate::ui::theme::State;
            cat.menu_agent_states[match state {
                State::Working => 0,
                State::Blocked => 1,
                State::Idle | State::Unknown => 2,
                State::Done => 3,
            }]
            .into()
        }
        AgentMenuItem::ToggleWorkspaceScope => if scoped {
            cat.menu_show_all_workspaces
        } else {
            cat.menu_show_workspace_only
        }
        .to_string(),
        AgentMenuItem::Resume => cat.menu_resume.to_string(),
        AgentMenuItem::RenamePane => cat.menu_rename.to_string(),
        AgentMenuItem::Pin => cat.menu_pin.to_string(),
        AgentMenuItem::Unpin => cat.menu_unpin.to_string(),
        AgentMenuItem::TogglePath => if paths_visible {
            cat.menu_hide_path
        } else {
            cat.menu_show_path
        }
        .to_string(),
        AgentMenuItem::OwnerActions => cat.settings.module_actions.to_string(),
        AgentMenuItem::Close => cap_first(cat.act_close),
        AgentMenuItem::AutomationDetails => cap_first(cat.act_details),
        AgentMenuItem::AutomationRun => cap_first(cat.automation_now),
        AgentMenuItem::AutomationToggle => cap_first(cat.board_automation_toggle),
        AgentMenuItem::AutomationDelete => cap_first(cat.act_delete),
        AgentMenuItem::Divider => String::new(),
        AgentMenuItem::Module(i) => module_label(extras, i),
    }
}

pub(super) fn draw_session_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.session_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let actions = menu.actions();
    let selected = menu.selected;
    let rows: Vec<MenuRow> = actions
        .iter()
        .map(|item| {
            use crate::app::SessionMenuItem::*;
            let text = match *item {
                Start(index) => format!(
                    "{} · {}",
                    cat.menu_start_session,
                    menu.targets[index].label(cat)
                ),
                Stop(index) => format!(
                    "{} · {}",
                    cat.menu_stop_session,
                    menu.targets[index].label(cat)
                ),
                Delete(index) | ConfirmDelete(index) => format!(
                    "{} · {}",
                    cap_first(cat.act_delete),
                    menu.targets[index].label(cat)
                ),
                Cancel => cap_first(cat.act_cancel),
                Explanation => cat.session_delete_confirm.to_string(),
            };
            MenuRow {
                text,
                divider: false,
                destructive: matches!(item, Stop(_) | Delete(_) | ConfirmDelete(_)),
            }
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected: app.hover.is_none().then_some(selected),
            mobile: app.compact,
            id: PopupId::Session,
            scroll: &mut app.menu_scroll,
        },
    );
    if let Some(menu) = app.session_menu.as_mut() {
        menu.items = actions.into_iter().zip(rects).collect();
    }
}

fn ws_label(
    it: WsMenuItem,
    cat: &Catalog,
    extras: &[ModuleMenuAction],
    paths_visible: bool,
) -> String {
    match it {
        WsMenuItem::Pin => cat.menu_pin.to_string(),
        WsMenuItem::Unpin => cat.menu_unpin.to_string(),
        WsMenuItem::TogglePath => if paths_visible {
            cat.menu_hide_path
        } else {
            cat.menu_show_path
        }
        .to_string(),
        // A trailing ▸ marks the row that opens the Quick Actions submenu.
        WsMenuItem::QuickActions => format!("{} ▸", cat.menu_quick_actions),
        WsMenuItem::CopyPath => cat.menu_copy_path.to_string(),
        WsMenuItem::CopyBranch => cat.menu_copy_branch.to_string(),
        WsMenuItem::Close => cap_first(cat.act_close),
        WsMenuItem::Rename => cat.menu_rename.to_string(),
        WsMenuItem::DeleteWorktree => cat.menu_delete_worktree.to_string(),
        WsMenuItem::NewWorktree => cat.menu_new_worktree.to_string(),
        WsMenuItem::OpenWorktree => cat.menu_open_worktree.to_string(),
        WsMenuItem::Divider => String::new(),
        WsMenuItem::OpenGit => cat.menu_open_git.to_string(),
        WsMenuItem::OpenOrch => cat.menu_open_board.to_string(),
        WsMenuItem::OpenMission => cat.mc_open.to_string(),
        WsMenuItem::OwnerSettings => cat.settings_title.to_string(),
        WsMenuItem::OwnerModules => cat.tab_modules.to_string(),
        WsMenuItem::Module(i) => module_label(extras, i),
    }
}

fn pane_label(it: PaneMenuItem, cat: &Catalog, extras: &[ModuleMenuAction]) -> String {
    match it {
        PaneMenuItem::NewPane => cat.menu_new_pane.to_string(),
        PaneMenuItem::SplitVertical => cat.menu_split_vertical.to_string(),
        PaneMenuItem::SplitHorizontal => cat.menu_split_horizontal.to_string(),
        PaneMenuItem::ForkPane => cat.menu_fork_pane.to_string(),
        PaneMenuItem::OpenLink => cat.menu_open_link.to_string(),
        PaneMenuItem::OpenFile => cat.menu_open_file.to_string(),
        PaneMenuItem::RunningCmd => cat.menu_running_cmd.to_string(),
        PaneMenuItem::RenamePane => cat.menu_rename.to_string(),
        PaneMenuItem::OpenMarkdownPreview => cat.menu_markdown_preview.to_string(),
        PaneMenuItem::OpenMermaidPreview => cat.menu_mermaid_preview.to_string(),
        // A trailing ▸ marks the row that opens the tabs submenu.
        PaneMenuItem::MoveToTab => format!("{} ▸", cat.menu_move_to_tab),
        PaneMenuItem::Divider => String::new(),
        PaneMenuItem::Close => cap_first(cat.act_close),
        PaneMenuItem::Module(i) => module_label(extras, i),
    }
}

fn tab_label(it: TabMenuItem, cat: &Catalog, extras: &[ModuleMenuAction]) -> String {
    match it {
        TabMenuItem::Rename => cat.menu_rename.to_string(),
        TabMenuItem::MoveLeft => format!("{} {}", cap_first(cat.act_move), cat.side_left),
        TabMenuItem::MoveRight => format!("{} {}", cap_first(cat.act_move), cat.side_right),
        TabMenuItem::SwapWith => format!("{} ▸", cat.tab_swap_with),
        TabMenuItem::Divider => String::new(),
        TabMenuItem::Module(i) => module_label(extras, i),
    }
}

/// A module action's row label. Module titles come from the module author, so
/// they are never translated — and a stale index renders blank rather than
/// panicking (the registry can change while a menu is open).
fn module_label(extras: &[ModuleMenuAction], i: usize) -> String {
    extras.get(i).map(|a| a.title.clone()).unwrap_or_default()
}

/// Uppercase the first character (no-op for scripts without case, e.g. CJK), so
/// the reused lower-case `act_close` reads as a menu label.
fn cap_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub(super) fn draw_file_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.file_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let editors = menu.editors.clone();
    let selected = menu.selected;
    let items = menu.build_items();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: file_label(*it, &editors, cat),
            divider: matches!(it, FileMenuItem::Divider),
            destructive: matches!(it, FileMenuItem::Delete),
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected,
            mobile: app.compact,
            id: PopupId::File,
            scroll: &mut app.menu_scroll,
        },
    );
    if let Some(menu) = app.file_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

pub(super) fn draw_diff_menu(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    let Some(menu) = app.diff_menu.as_ref() else {
        return;
    };
    let items = crate::app::DiffMenu::ITEMS;
    let selected = menu.selected;
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|item| MenuRow {
            text: match item {
                DiffMenuItem::OpenPreview => "Open Preview",
                DiffMenuItem::OpenPane => "Open in Pane",
                DiffMenuItem::OpenTab => "Open in Tab",
                DiffMenuItem::CopyPath => "Copy Path",
            }
            .to_string(),
            divider: false,
            destructive: false,
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        menu.anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected,
            mobile: app.compact,
            id: PopupId::Diff,
            scroll: &mut app.menu_scroll,
        },
    );
    if let Some(menu) = app.diff_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

pub(super) fn draw_orch_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.orch_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let task = menu.task.clone();
    let items = app.orch_menu_items(&task);
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|item| MenuRow {
            text: orch_label(*item, cat),
            divider: matches!(item, OrchMenuItem::Divider),
            destructive: matches!(item, OrchMenuItem::Delete),
        })
        .collect();
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected: None,
            mobile: app.compact,
            id: PopupId::Orch,
            scroll: &mut app.menu_scroll,
        },
    );
    if let Some(menu) = app.orch_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

fn orch_label(item: OrchMenuItem, cat: &Catalog) -> String {
    match item {
        OrchMenuItem::Start => cap_first(cat.board_start),
        OrchMenuItem::Jump => cap_first(cat.scroll_jump),
        OrchMenuItem::Details => cap_first(cat.board_details),
        OrchMenuItem::Done => cap_first(cat.task_done),
        OrchMenuItem::Retry => cap_first(cat.board_retry),
        OrchMenuItem::Merge => cap_first(cat.act_merge),
        OrchMenuItem::Release => cap_first(cat.board_release),
        OrchMenuItem::CopyId => format!("{} ID", cap_first(cat.act_copy)),
        OrchMenuItem::CopyWorktree => {
            format!("{} {}", cap_first(cat.act_copy), cat.board_f_paths)
        }
        OrchMenuItem::Divider => String::new(),
        OrchMenuItem::Delete => cap_first(cat.act_delete),
    }
}

/// FILES-menu labels are plain English (this menu is not localized — unlike the
/// workspace/pane menus — and editor names are proper nouns anyway).
fn file_label(it: FileMenuItem, editors: &[(String, String)], cat: &Catalog) -> String {
    match it {
        FileMenuItem::OpenReadonly => "Open in Tab".to_string(),
        FileMenuItem::OpenWith(i) => editors
            .get(i)
            .map(|(_, label)| format!("Open in {label}"))
            .unwrap_or_default(),
        FileMenuItem::OpenMarkdownPreview => cat.menu_markdown_preview.to_string(),
        FileMenuItem::OpenMermaidPreview => cat.menu_mermaid_preview.to_string(),
        FileMenuItem::NewFile => "New File".to_string(),
        FileMenuItem::NewFolder => "New Folder".to_string(),
        FileMenuItem::Rename => "Rename".to_string(),
        FileMenuItem::CopyPath => "Copy Path".to_string(),
        FileMenuItem::InsertPath => "Insert Path".to_string(),
        FileMenuItem::OpenAsNewWorkspace => "Open as Workspace".to_string(),
        FileMenuItem::Divider => String::new(),
        FileMenuItem::Delete => "Delete".to_string(),
    }
}

/// The context menu a module declared for one of its dock rows (docs/52).
///
/// Unlike the other menus this renders a snapshot rather than recomputing its
/// items — the live dock rows may already have been replaced underneath it.
pub(super) fn draw_dock_menu(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    let Some(menu) = app.dock_menu.as_ref() else {
        return;
    };
    let rows: Vec<MenuRow> = menu
        .items
        .iter()
        .map(|it| MenuRow {
            text: it.title.clone(),
            divider: it.is_divider(),
            destructive: it.destructive,
        })
        .collect();
    let anchor = menu.anchor;
    let rects = render_popup(
        f,
        area,
        anchor,
        &rows,
        t,
        PopupCtx {
            hover: app.hover,
            selected: None,
            mobile: app.compact,
            id: PopupId::Dock,
            scroll: &mut app.menu_scroll,
        },
    );
    if let Some(menu) = app.dock_menu.as_mut() {
        menu.rects = rects;
    }
}

#[cfg(test)]
mod label_case_tests {
    use super::*;
    use crate::app::{AgentMenuItem, FileMenuItem, PaneMenuItem, TabMenuItem, WsMenuItem};

    /// Words that stay lower-case inside a title, unless they lead it.
    const MINOR: [&str; 12] = [
        "a", "an", "the", "to", "in", "on", "of", "for", "and", "or", "with", "as",
    ];

    #[test]
    fn rounded_popup_preserves_outer_corners_and_rounds_the_selected_row() {
        let area = Rect::new(0, 0, 50, 20);
        let mut buffer = Buffer::empty(area);
        let mut target = RenderTarget::new(&mut buffer, area);
        target.rounded_corners = true;
        let theme = Theme::noir();
        let canvas = Color::Rgb(245, 245, 245);
        target.buffer_mut().set_style(area, Style::new().bg(canvas));
        let mut scroll = MenuScroll::default();
        let rows = [MenuRow {
            text: "Create pane".into(),
            divider: false,
            destructive: false,
        }];
        let hits = render_popup(
            &mut target,
            area,
            (4, 5),
            &rows,
            &theme,
            PopupCtx {
                hover: None,
                selected: Some(0),
                mobile: false,
                id: PopupId::Pane,
                scroll: &mut scroll,
            },
        );
        assert_eq!(buffer[(4, 5)].symbol(), "╭");
        assert_eq!(buffer[(4, 5)].bg, canvas);
        let row = hits[0];
        assert_eq!(buffer[(row.x, row.y)].symbol(), "◖");
        assert_eq!(buffer[(row.right() - 1, row.y)].symbol(), "◗");
        assert_eq!(buffer[(row.x + 1, row.y)].fg, theme.on_color(theme.accent));
        let text: String = (row.x..row.right())
            .map(|x| buffer[(x, row.y)].symbol())
            .collect();
        assert!(text.contains("Create pane"), "{text}");
    }

    #[test]
    fn mobile_menu_hover_covers_the_full_touch_row() {
        let row = Rect::new(4, 7, 20, 2);
        assert!(row_is_hovered(row, Some((5, 7))));
        assert!(row_is_hovered(row, Some((5, 8))));
        assert!(!row_is_hovered(row, Some((5, 9))));
        assert!(!row_is_hovered(row, Some((24, 8))));
    }

    /// Every context-menu row reads as **Title Case**: each word capitalized bar
    /// the short articles/prepositions, which never lead. Hyphenated parts count
    /// as words of their own ("Read-Only"), and trailing marks like the submenu
    /// `▸` are ignored.
    fn offending_word(label: &str) -> Option<String> {
        let mut lead = true;
        for word in label.split_whitespace() {
            let word = word.trim_matches(|c: char| !c.is_alphanumeric());
            if word.is_empty() {
                continue;
            }
            for part in word.split('-') {
                let Some(first) = part.chars().find(|c| c.is_alphabetic()) else {
                    continue;
                };
                let minor = MINOR.contains(&part.to_lowercase().as_str());
                if !first.is_uppercase() && (!minor || lead) {
                    return Some(part.to_string());
                }
                lead = false;
            }
        }
        None
    }

    /// The rule the check itself relies on — a guard against it silently passing
    /// everything (it would, if `offending_word` stopped looking at words).
    #[test]
    fn the_title_case_check_rejects_sentence_case() {
        assert_eq!(offending_word("Open Task Board"), None);
        assert_eq!(offending_word("Fork to New Pane"), None);
        assert_eq!(offending_word("Open in Tab"), None);
        assert_eq!(offending_word("Move to Tab ▸"), None);
        assert_eq!(offending_word("Open task board").as_deref(), Some("task"));
        assert_eq!(offending_word("Open (read-only)").as_deref(), Some("read"));
        assert_eq!(offending_word("to Open").as_deref(), Some("to"));
    }

    /// One casing standard across every context menu, so the workspace menu can't
    /// drift into "Open Mission Control" beside "Open task board" again.
    #[test]
    fn every_english_context_menu_row_is_title_case() {
        let cat = &crate::i18n::EN;
        let none: &[ModuleMenuAction] = &[];
        let editors = [("nvim".to_string(), "Neovim".to_string())];

        let mut rows: Vec<String> = Vec::new();
        for it in [
            WsMenuItem::Close,
            WsMenuItem::Rename,
            WsMenuItem::TogglePath,
            WsMenuItem::QuickActions,
            WsMenuItem::CopyPath,
            WsMenuItem::CopyBranch,
            WsMenuItem::DeleteWorktree,
            WsMenuItem::NewWorktree,
            WsMenuItem::OpenWorktree,
            WsMenuItem::OpenGit,
            WsMenuItem::OpenOrch,
            WsMenuItem::OpenMission,
        ] {
            rows.push(ws_label(it, cat, none, true));
        }
        for it in PaneMenuItem::ALL.iter().copied() {
            rows.push(pane_label(it, cat, none));
        }
        for it in [
            TabMenuItem::Rename,
            TabMenuItem::MoveLeft,
            TabMenuItem::MoveRight,
            TabMenuItem::SwapWith,
        ] {
            rows.push(tab_label(it, cat, none));
        }
        // The "Move to Tab" submenu is part of the pane menu: its tab rows are
        // user content, but the trailing "New Tab" is ours (`move_targets` in
        // `app/mod.rs`).
        rows.push(cat.menu_new_tab.to_string());
        for it in [
            AgentMenuItem::Resume,
            AgentMenuItem::TogglePath,
            AgentMenuItem::Close,
        ] {
            rows.push(agent_label(it, cat, none, false, true));
        }
        for it in [
            OrchMenuItem::Start,
            OrchMenuItem::Jump,
            OrchMenuItem::Details,
            OrchMenuItem::Done,
            OrchMenuItem::Retry,
            OrchMenuItem::Merge,
            OrchMenuItem::Release,
            OrchMenuItem::CopyId,
            OrchMenuItem::CopyWorktree,
            OrchMenuItem::Divider,
            OrchMenuItem::Delete,
        ] {
            rows.push(orch_label(it, cat));
        }
        for it in [
            FileMenuItem::OpenReadonly,
            FileMenuItem::OpenWith(0),
            FileMenuItem::OpenMarkdownPreview,
            FileMenuItem::OpenMermaidPreview,
            FileMenuItem::NewFile,
            FileMenuItem::NewFolder,
            FileMenuItem::Rename,
            FileMenuItem::CopyPath,
            FileMenuItem::InsertPath,
            FileMenuItem::OpenAsNewWorkspace,
            FileMenuItem::Delete,
        ] {
            rows.push(file_label(it, &editors, cat));
        }

        let bad: Vec<String> = rows
            .iter()
            .filter(|r| !r.is_empty())
            .filter_map(|r| offending_word(r).map(|w| format!("{r:?} (word {w:?})")))
            .collect();
        assert!(bad.is_empty(), "menu rows are not Title Case: {bad:#?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, FileMenu, FileMenuItem, PopupId, SessionMenu, SessionMenuItem};
    use crate::event::AppEvent;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;

    #[test]
    fn session_delete_confirmation_clicks_stay_inside_desktop_and_mobile_viewports() {
        use crate::app::{SessionMenu, SessionMenuItem};
        let _env = crate::persist::test_env("session-delete-popup-geometry");
        for compact in [false, true] {
            let mut app = crate::app::remote::tests::remote_ui_app();
            app.compact = compact;
            let area = Rect::new(7, 3, 100, 18);
            app.session_menu = Some(SessionMenu {
                anchor: (area.right() - 1, area.bottom() - 1),
                items: Vec::new(),
                selected: 1,
                confirming: Some(0),
                targets: vec![crate::app::session_menu::SessionMenuTarget {
                    name: "saved".into(),
                    remote: Some(
                        crate::session::remote::RemoteSession::new("build", "saved").unwrap(),
                    ),
                    running: false,
                    current: false,
                }],
            });
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            let theme = app.theme.clone();
            let catalog = app.catalog;
            draw_session_menu(
                &mut RenderTarget::new(&mut buffer, area),
                area,
                &mut app,
                catalog,
                &theme,
            );
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("Delete · build · saved"));
            assert!(text.contains("Project files are kept."));
            let menu = app.session_menu.as_ref().unwrap();
            for (_, rect) in &menu.items {
                assert!(rect.x >= area.x && rect.right() <= area.right());
                assert!(rect.y >= area.y && rect.bottom() <= area.bottom());
                assert_eq!(rect.height, if compact { 2 } else { 1 });
            }
            let cancel = menu
                .items
                .iter()
                .find(|(item, _)| *item == SessionMenuItem::Cancel)
                .unwrap()
                .1;
            app.session_menu_click(cancel.x, cancel.bottom() - 1);
            assert!(
                app.session_menu.is_none(),
                "both rows of the compact Cancel target are actionable"
            );
        }
    }

    /// A FILES menu with two editors is ten rows, which does not fit a ten-row
    /// terminal — the shape that used to lose its last rows outright.
    fn app_with_file_menu(height: u16) -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, height, tx).unwrap();
        app.file_menu = Some(file_menu());
        app
    }

    fn file_menu() -> FileMenu {
        FileMenu {
            path: std::env::temp_dir().join("scroll.txt"),
            is_dir: false,
            anchor: (10, 0),
            items: Vec::new(),
            selected: None,
            editors: vec![
                ("vim".to_string(), "Vim".to_string()),
                ("hx".to_string(), "Helix".to_string()),
            ],
        }
    }

    /// The rect the input layer would hit-test for `item`. An empty one means the
    /// row is out of view, and so unreachable.
    fn rect_of(app: &App, item: FileMenuItem) -> Rect {
        app.file_menu
            .as_ref()
            .expect("menu open")
            .items
            .iter()
            .find(|(it, _)| *it == item)
            .map(|(_, rect)| *rect)
            .expect("row is in the menu")
    }

    fn mouse(app: &mut App, kind: MouseEventKind, at: (u16, u16)) {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind,
            column: at.0,
            row: at.1,
            modifiers: KeyModifiers::NONE,
        }));
    }

    #[test]
    fn a_menu_taller_than_its_space_scrolls_instead_of_dropping_its_last_rows() {
        let _env = crate::persist::test_env("menu-scroll-reach");
        let mut app = app_with_file_menu(10);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();

        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let top = rect_of(&app, FileMenuItem::OpenReadonly);
        assert!(top.height > 0, "the first row is on screen");
        assert_eq!(
            rect_of(&app, FileMenuItem::Delete).height,
            0,
            "Delete does not fit yet, so it has no rect to click"
        );

        // One notch moves the list by one row.
        let over = (top.x + 1, top.y);
        mouse(&mut app, MouseEventKind::ScrollDown, over);
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.menu_scroll.offset_of(PopupId::File), 1);

        // Then to the bottom. Deliberately more notches than this menu has rows:
        // the offset clamps, and the test should not have to be edited every time
        // the FILES menu grows a row.
        for _ in 0..20 {
            mouse(&mut app, MouseEventKind::ScrollDown, over);
        }
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        let delete = rect_of(&app, FileMenuItem::Delete);
        assert!(
            delete.height > 0,
            "the wheel brought the last row into view — this is the bug"
        );

        // And it is genuinely clickable, not merely painted: the rect the render
        // handed back is the one the input layer acts on.
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (delete.x + 1, delete.y),
        );
        assert!(
            app.file_delete.is_some(),
            "clicking the scrolled-in row ran its action"
        );
    }

    #[test]
    fn keyboard_selection_keeps_the_file_action_in_view() {
        let _env = crate::persist::test_env("menu-keyboard-reveal");
        let mut app = app_with_file_menu(10);
        let selected = app
            .file_menu
            .as_ref()
            .unwrap()
            .build_items()
            .iter()
            .position(|item| *item == FileMenuItem::Delete)
            .unwrap();
        app.file_menu.as_mut().unwrap().selected = Some(selected);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();

        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        assert!(app.menu_scroll.offset_of(PopupId::File) > 0);
        assert!(
            rect_of(&app, FileMenuItem::Delete).height > 0,
            "the selected keyboard action is rendered and reachable"
        );
    }

    #[test]
    fn menu_scroll_stops_at_both_ends() {
        let _env = crate::persist::test_env("menu-scroll-clamp");
        let mut app = app_with_file_menu(10);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let over = {
            let top = rect_of(&app, FileMenuItem::OpenReadonly);
            (top.x + 1, top.y)
        };

        for _ in 0..20 {
            mouse(&mut app, MouseEventKind::ScrollDown, over);
        }
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let bottom = app.menu_scroll.offset_of(PopupId::File);
        assert!(bottom > 0, "it scrolled");
        assert!(
            rect_of(&app, FileMenuItem::Delete).height > 0,
            "the last row is in view and the list cannot run past it"
        );

        for _ in 0..20 {
            mouse(&mut app, MouseEventKind::ScrollUp, over);
        }
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(
            app.menu_scroll.offset_of(PopupId::File),
            0,
            "and back to the first row, not past it"
        );
        assert!(rect_of(&app, FileMenuItem::OpenReadonly).height > 0);
    }

    #[test]
    fn the_wheel_away_from_a_popup_leaves_it_alone() {
        let _env = crate::persist::test_env("menu-scroll-elsewhere");
        let mut app = app_with_file_menu(10);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        // Far from the popup, which is anchored at column 10.
        mouse(&mut app, MouseEventKind::ScrollDown, (70, 8));
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(
            app.menu_scroll.offset_of(PopupId::File),
            0,
            "the wheel belongs to whatever is under it, not to the open menu"
        );
    }

    /// A submenu is anchored beside its parent, but clamped back on screen at
    /// the right edge — where it covers the parent it belongs to. The wheel goes
    /// to what is on top, which is the one drawn last.
    #[test]
    fn an_overlapping_submenu_takes_the_wheel_from_the_menu_it_covers() {
        let mut scroll = crate::app::MenuScroll::default();
        let parent = Rect::new(40, 0, 20, 10);
        let submenu = Rect::new(45, 0, 20, 10); // clamped left, over the parent
        scroll.record(PopupId::Pane, parent, 5);
        scroll.record(PopupId::PaneMove, submenu, 5);

        assert!(scroll.wheel(50, 3, 1), "the overlap belongs to a popup");
        assert_eq!(scroll.offset_of(PopupId::PaneMove), 1, "the visible one");
        assert_eq!(scroll.offset_of(PopupId::Pane), 0, "not the covered one");

        // And the parent still takes the wheel where it is not covered.
        assert!(scroll.wheel(41, 3, 1));
        assert_eq!(scroll.offset_of(PopupId::Pane), 1);
    }

    #[test]
    fn reopening_a_menu_starts_it_at_the_top() {
        let _env = crate::persist::test_env("menu-scroll-reopen");
        let mut app = app_with_file_menu(10);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let over = {
            let top = rect_of(&app, FileMenuItem::OpenReadonly);
            (top.x + 1, top.y)
        };
        mouse(&mut app, MouseEventKind::ScrollDown, over);
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(app.menu_scroll.offset_of(PopupId::File) > 0, "scrolled");

        // A press away from the popup is what dismisses one menu and opens the
        // next, so the next one starts where a menu should.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Right), (70, 8));
        app.file_menu = Some(file_menu());
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.menu_scroll.offset_of(PopupId::File), 0);
        assert!(rect_of(&app, FileMenuItem::OpenReadonly).height > 0);
    }

    #[test]
    fn stopped_session_menu_renders_start_and_delete_for_keyboard_and_mouse() {
        let _env = crate::persist::test_env("session-actions-render");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 20, tx).unwrap();
        app.session_menu = Some(SessionMenu {
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
        let area = Rect::new(0, 0, 80, 20);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        let mut target = crate::ui::RenderTarget::new(&mut buffer, area);
        let theme = app.theme.clone();
        let catalog = app.catalog;

        draw_session_menu(&mut target, area, &mut app, catalog, &theme);

        let items = &app.session_menu.as_ref().unwrap().items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, SessionMenuItem::Start(0));
        assert_eq!(items[1].0, SessionMenuItem::Delete(0));
        assert_ne!(items[0].1, items[1].1);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains(catalog.menu_start_session));
        assert!(text.contains("Delete"));
        assert_eq!(
            buffer.cell((items[0].1.x, items[0].1.y)).unwrap().bg,
            theme.accent,
            "the keyboard-selected Start row is highlighted"
        );
    }
}

/// Reuse the ordinary context menu and wrapped diagnostic text renderer.
pub(super) fn draw_host_inspect(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(inspect) = app.host_inspect.as_ref() else {
        return;
    };
    if let Some(report) = &inspect.details {
        help::dim_backdrop(f, area, t);
        let modal = help::centered_rect(
            area,
            area.width.saturating_sub(4).min(110),
            area.height.saturating_sub(2).min(32),
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(f.border_type())
            .title(if inspect.errors_only {
                cat.host_errors
            } else {
                cat.host_details
            })
            .style(Style::new().fg(t.text).bg(t.surface0))
            .border_style(Style::new().fg(t.border_focus));
        let inner = block.inner(modal);
        f.render_widget(Clear, modal);
        f.render_widget(block, modal);
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(2),
        );
        let lines: Vec<_> = report
            .lines()
            .flat_map(|line| super::board::wrap_display_lines(line, body.width as usize))
            .collect();
        let max_scroll = lines.len().saturating_sub(body.height as usize);
        let scroll = inspect.scroll.min(max_scroll);
        let text: Vec<_> = lines
            .into_iter()
            .skip(scroll)
            .take(body.height as usize)
            .map(Line::from)
            .collect();
        f.render_widget(Paragraph::new(text), body);
        if inner.height > 0 {
            f.render_widget(
                Paragraph::new(cat.host_detail_hint).style(Style::new().fg(t.overlay1)),
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            );
        }
        if let Some(inspect) = app.host_inspect.as_mut() {
            inspect.modal = Some(modal);
            inspect.max_scroll = max_scroll;
        }
    } else {
        let rows: Vec<_> = [cat.host_details, cat.host_errors, cat.host_copy]
            .into_iter()
            .map(|text| MenuRow {
                text: text.into(),
                divider: false,
                destructive: false,
            })
            .collect();
        let rects = render_popup(
            f,
            area,
            inspect.anchor,
            &rows,
            t,
            PopupCtx {
                hover: app.hover,
                selected: Some(inspect.selected),
                mobile: app.compact,
                id: PopupId::Host,
                scroll: &mut app.menu_scroll,
            },
        );
        if let Some(inspect) = app.host_inspect.as_mut() {
            inspect.rects = rects;
        }
    }
}
