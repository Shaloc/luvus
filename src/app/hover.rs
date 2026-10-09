//! Delayed, read-only labels over existing hit geometry. The existing runtime
//! deadline wakes once after hover-enter; shown labels need no idle timer.
use super::{App, State};
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) struct ChromeHover {
    pub rect: Rect,
    pub ready_at: Instant,
    pub visible: bool,
}

impl App {
    fn state_hint(&self, state: State) -> &str {
        match state {
            State::Working => self.catalog.menu_agent_states[0],
            State::Blocked => self.catalog.menu_agent_states[1],
            State::Idle => self.catalog.menu_agent_states[2],
            State::Done => self.catalog.menu_agent_states[3],
            State::Unknown => "—",
        }
    }

    pub(crate) fn chrome_hint_at(&self, at: Option<(u16, u16)>) -> Option<(Rect, String)> {
        let at = at?;
        if !crate::ui::chrome_uncovered(self)
            || self.copy_mode.is_some()
            || self.mouse_grab.is_some()
            || self.remote_mouse_capture.is_some()
            || self.selection.as_ref().is_some_and(|s| s.dragging)
        {
            return None;
        }
        let hit = |rect: Rect| rect.contains(at.into());
        for &(pane, right_click, rect) in &self.pane_mouse_rects {
            if hit(rect) {
                let options = self.panes.get(&pane)?.mouse_options;
                let on = if right_click {
                    options.right_click_to_app
                } else {
                    options.copy_on_select
                };
                return Some((
                    rect,
                    format!(
                        "{} {}",
                        if on { "✓" } else { "○" },
                        self.catalog.pane_mouse_hints[usize::from(!right_click)]
                    ),
                ));
            }
        }
        for (rect, label) in [
            (self.pane_restart_rect, self.catalog.pane_restart),
            (self.pane_close_rect, self.catalog.act_close),
            (self.pane_zoom_rect, self.catalog.cmd_zoom_pane),
        ] {
            if let Some(rect) = rect.filter(|rect| hit(*rect)) {
                return Some((rect, label.to_owned()));
            }
        }
        for &(index, rect) in self.ws_rects.iter().chain(self.agent_group_rects.iter()) {
            if hit(rect) {
                let ws = self.workspaces.get(index)?;
                let source = ws
                    .remote
                    .as_ref()
                    .map(|r| format!("{} / {} · ", r.host, r.session))
                    .unwrap_or_default();
                return Some((
                    rect,
                    format!(
                        "{source}{} · {} · {}",
                        ws.name,
                        self.state_hint(crate::ui::workspace_state(self, index)),
                        ws.cwd.display()
                    ),
                ));
            }
        }
        for (target, pane, rect) in &self.remote_agent_rects {
            if hit(*rect) {
                let view = self
                    .workspaces
                    .iter()
                    .position(|w| w.remote.as_ref() == Some(target))
                    .and_then(|i| self.remote_workspace_view(i))?;
                let agent = view.agents.iter().find(|a| &a.pane == pane)?;
                return Some((
                    *rect,
                    format!(
                        "{} / {} · {} · {} · {}",
                        target.host,
                        target.session,
                        agent
                            .name
                            .as_deref()
                            .or(agent.title.as_deref())
                            .unwrap_or(&agent.agent),
                        self.state_hint(agent.state),
                        agent.cwd
                    ),
                ));
            }
        }
        let pane_hint = |id, rect| {
            let pane = self.panes.get(&id)?;
            let status = self.status.get(&id);
            let title = self
                .agent_name_for(id)
                .map(ToOwned::to_owned)
                .or_else(|| self.pane_title(id))
                .unwrap_or_else(|| pane.cwd.display().to_string());
            Some((
                rect,
                format!(
                    "{} · {} · {}",
                    title,
                    self.state_hint(status.map_or(State::Unknown, |s| s.state)),
                    pane.cwd.display()
                ),
            ))
        };
        for &(pane, rect) in self.agent_rects.iter().chain(self.pane_title_rects.iter()) {
            if hit(rect) {
                return pane_hint(pane, rect);
            }
        }
        for &(index, rect) in &self.session_rects {
            if hit(rect) {
                let session = self.resumable.get(index)?;
                return Some((
                    rect,
                    format!(
                        "{} · {} · {}",
                        session.agent,
                        self.agent_row_title_for_session(&session.agent, &session.session_id)
                            .unwrap_or(&session.agent),
                        session.cwd.display()
                    ),
                ));
            }
        }
        for (pane, key, rect) in &self.remote_history_rects {
            if hit(*rect) {
                let super::ViewKind::Remote(view) = self.views.get(pane)? else {
                    return None;
                };
                let session = view.history.iter().find(|s| &s.key == key)?;
                return Some((
                    *rect,
                    format!(
                        "{} / {} · {} · {}",
                        view.target.host,
                        view.target.session,
                        session.title.as_deref().unwrap_or(&session.agent),
                        session.cwd
                    ),
                ));
            }
        }
        // The unbordered single pane has a header but no split-title hit entry.
        for &(pane, rect) in &self.pane_rects {
            if rect.y == at.1
                && hit(rect)
                && self
                    .pane_content_rects
                    .iter()
                    .any(|(id, content)| *id == pane && content.y > rect.y)
            {
                return pane_hint(pane, Rect::new(rect.x, rect.y, rect.width, 1));
            }
        }
        None
    }

    pub(crate) fn update_chrome_hover(&mut self, now: Instant) {
        let rect = self.chrome_hint_at(self.hover).map(|(rect, _)| rect);
        if self.chrome_hover.map(|h| h.rect) != rect {
            self.chrome_hover = rect.map(|rect| ChromeHover {
                rect,
                ready_at: now + Duration::from_millis(350),
                visible: false,
            });
        }
    }

    pub(crate) fn tick_chrome_hover(&mut self, now: Instant) -> bool {
        let Some(hover) = self.chrome_hover else {
            return false;
        };
        if self
            .chrome_hint_at(self.hover)
            .is_none_or(|(r, _)| r != hover.rect)
        {
            self.chrome_hover = None;
            return true;
        }
        if !hover.visible && now >= hover.ready_at {
            self.chrome_hover.as_mut().unwrap().visible = true;
            return true;
        }
        false
    }
}
