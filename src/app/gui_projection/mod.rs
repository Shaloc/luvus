mod files;
mod palette;
pub(crate) mod registered;
mod repository;
mod views;
mod widgets;

use serde_json::{json, Value};

use super::{App, Cmd, DockKind, Side, ViewKind};
use crate::ids::PaneId;
use crate::ipc::app_surface::protocol::{Cell, Cursor, Target, TerminalFrame};
use crate::ipc::protocol::pack;
use crate::terminal::vt::VtEngine;
use views::Budget;

const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 300;
const MAX_FRAME_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// One client's history anchor for one terminal lifetime, never shared or persisted.
#[derive(Default)]
pub(crate) struct HistoryViewport {
    offset: usize,
    /// Last observed (history epoch, row scroll sequence, geometry revision).
    observed: Option<(u64, u64, u64)>,
}

impl HistoryViewport {
    /// Resolve only while holding the same engine lock used to read the rows.
    pub(crate) fn resolve(&mut self, engine: &dyn VtEngine) -> usize {
        if engine.alt_screen() {
            // The alternate grid has unrelated counters and is always shown live.
            *self = Self::default();
            return 0;
        }
        let (epoch, sequence) = engine.history_scroll_state();
        let geometry = engine.geometry_revision();
        if let Some((last_epoch, last_sequence, last_geometry)) = self.observed {
            if epoch != last_epoch {
                self.offset = 0;
            } else if self.offset != 0 && geometry == last_geometry {
                let delta =
                    usize::try_from(sequence.wrapping_sub(last_sequence)).unwrap_or(usize::MAX);
                self.offset = self.offset.saturating_add(delta);
            }
        }
        self.offset = self.offset.min(engine.history_len());
        // Resize retains relative position without counting its internal rotations twice.
        self.observed = Some((epoch, sequence, geometry));
        self.offset
    }

    pub(crate) fn scroll(&mut self, engine: &dyn VtEngine, lines: i16) {
        let offset = self.resolve(engine);
        if !engine.alt_screen() {
            self.offset = offset
                .saturating_add_signed(isize::from(lines))
                .min(engine.history_len());
        }
    }
}

impl App {
    pub(crate) fn gui_snapshot(&self) -> Value {
        let mut snapshot = self.runtime_snapshot();
        snapshot["event_sequence"] =
            json!(crate::ipc::api::current_sequence(&self.events).to_string());
        let mut budget = Budget::default();
        for (wi, workspace) in self.workspaces.iter().enumerate() {
            let ws = &mut snapshot["workspaces"][wi];
            if workspace.remote.is_some() {
                self.project_registered_workspace(wi, ws);
                continue;
            }
            ws["gui_available"] = Value::Bool(true);
            for (ti, tab) in workspace.tabs.iter().enumerate() {
                let out = &mut ws["tabs"][ti];
                out["id"] = json!(tab.id);
                out["layout"] = json!(tab.layout.to_tree());
                out["focus"] = json!(tab.layout.focus.0.to_string());
                out["zoomed"] =
                    json!(self.zoomed && wi == self.active_ws && ti == workspace.active_tab);
                for (pi, id) in tab.layout.leaves().into_iter().enumerate() {
                    let leaf = &mut out["panes"][pi];
                    let view = self.views.get(&id);
                    if let Some(view @ ViewKind::Remote(_)) = view {
                        let mut remote = views::native(view, &mut budget);
                        remote["pane_id"] = json!(id.0.to_string());
                        remote["target"] = Value::Null;
                        *leaf = remote;
                        continue;
                    }
                    let terminal = self.panes.get(&id);
                    if terminal.is_some() && view.is_some() {
                        *leaf = views::unavailable(
                            "unknown",
                            "Unavailable",
                            "ambiguous_leaf_ownership",
                        );
                        leaf["pane_id"] = json!(id.0.to_string());
                        continue;
                    }
                    let terminal_id = terminal
                        .and_then(|pane| pane.terminal_runtime())
                        .map(|r| r.terminal_id);
                    if terminal.is_some() {
                        leaf["terminal_id"] = json!(terminal_id);
                    }
                    leaf["target"] = json!(Target {
                        generation: self.backend_server_generation.clone(),
                        workspace_id: workspace.id.clone(),
                        tab_id: tab.id.clone(),
                        pane_id: id.0.to_string(),
                        terminal_id,
                    });
                    if let Some(view) = view {
                        let native = views::native(view, &mut budget);
                        if let Value::Object(fields) = native {
                            leaf.as_object_mut().expect("snapshot leaf").extend(fields);
                        }
                    } else if let Some(pane) = terminal {
                        leaf["can_fork"] = json!(self
                            .status
                            .get(&id)
                            .is_some_and(
                                |s| s.agent_session.is_some() && crate::agent::can_fork(&s.agent)
                            ));
                        leaf["mouse_options"] = json!({"right_click_to_app":pane.mouse_options.right_click_to_app,"copy_on_select":pane.mouse_options.copy_on_select});
                        let (cols, rows) = pane.size();
                        let modes = pane.engine.try_lock().ok().map(|engine| json!({
                            "alternate_screen":engine.alt_screen(), "application_cursor":engine.application_cursor(),
                            "bracketed_paste":engine.bracketed_paste(), "mouse_reporting":engine.mouse_report(),
                            "mouse_drag":engine.mouse_drag(), "mouse_motion":engine.mouse_motion(), "sgr_mouse":engine.sgr_mouse(),
                        }));
                        leaf["terminal"] = json!({
                            "cols":cols, "rows":rows, "modes":modes,
                            "frame_supported":(1..=MAX_COLS).contains(&cols) && (1..=MAX_ROWS).contains(&rows),
                        });
                    } else {
                        let kind = if tab.is_git() {
                            "git"
                        } else if tab.is_orch() {
                            "orchestration"
                        } else if tab.is_mission() {
                            "mission_control"
                        } else {
                            "unknown"
                        };
                        leaf["kind"] = json!(kind);
                        leaf["title"] = json!(kind);
                        leaf["data"] = json!({"state":"unavailable", "reason":"semantic_dashboard_not_projected"});
                        leaf["target"] = Value::Null;
                    }
                }
            }
        }
        snapshot["config"] = json!(self.config);
        // Reuse discovery evidence already owned by the core; opening the sidebar must not dial SSH.
        snapshot["host_connections"] = json!(self
            .remote_host_status
            .iter()
            .map(|host| {
                let state = if host.error.is_some() {
                    "disconnected"
                } else if host.sessions.is_empty() {
                    "unknown"
                } else {
                    "connected"
                };
                json!({"host":host.host,"state":state,"sessions":host.sessions.iter().map(|s| &s.name).collect::<Vec<_>>()})
            })
            .collect::<Vec<_>>());
        snapshot["notification_inbox"] = json!({
            "revision": self.bar.history_revision.to_string(),
            "unread": self.bar.history.iter().filter(|row| row.unread).count(),
        });
        snapshot["read_only_workers"] = json!(crate::agent::registry::descriptors()
            .iter()
            .filter(|d| d.automation.is_some_and(|a| a.read_only.is_some()))
            .map(|d| d.id)
            .collect::<Vec<_>>());
        snapshot["desktop_features"] = json!(["browser_tab", "blank_browser_tab", "mouse_options"]);
        snapshot["terminal_palette"] = json!({
            "foreground":pack(self.theme.text), "background":pack(self.theme.base),
            "ansi":palette::ansi(&self.config.theme, &self.theme),
        });
        snapshot["commands"] = json!(Cmd::ALL
            .iter()
            .map(|cmd| json!({
                "id":cmd.id(), "label":cmd.label(self.catalog),
            }))
            .collect::<Vec<_>>());
        let prefix: std::collections::BTreeMap<_, _> = self
            .keymap
            .iter()
            .map(|(key, cmd)| (key, cmd.id()))
            .collect();
        let direct: std::collections::BTreeMap<_, _> = self
            .direct_keymap
            .iter()
            .filter(|(key, _)| !key.is_reserved_by(&self.prefix))
            .map(|(key, cmd)| (key.label(), cmd.id()))
            .collect();
        snapshot["key_bindings"] = json!({"prefix":self.prefix.spec(), "prefix_commands":prefix, "direct_commands":direct});
        snapshot["docks"] = self.gui_docks(&mut budget);
        let widgets: Vec<_> = self
            .bar
            .widgets
            .iter()
            .take(crate::bar::MAX_WIDGETS)
            .collect();
        snapshot["bars"] = json!({
            "config":self.config.bars, "total":self.bar.widgets.len(),
            "truncated":widgets.len() < self.bar.widgets.len(),
            "widgets":widgets.into_iter().map(|(id, widget)| json!({
                "id":id, "owner":widget.key.owner, "title":self.bar.title(id),
                "region":self.config.bars.region_for(id, widget.region), "priority":widget.priority,
                "content":widget.content, "compact_content":widget.compact_content,
            })).collect::<Vec<_>>(),
        });
        snapshot["gui_limits"] = json!({
            "rows_per_page":views::ROW_LIMIT, "terminal_cols":MAX_COLS, "terminal_rows":MAX_ROWS,
            "terminal_text_bytes":MAX_FRAME_TEXT_BYTES, "native_text_bytes":4 * 1024 * 1024,
            "native_items":16_384, "pagination_requests_supported":false,
            "oversized_terminal_frames":"omitted", "terminal_modes_in_snapshot":true,
        });
        snapshot
    }

    pub(crate) fn gui_terminal_frames(
        &self,
        offsets: &mut std::collections::HashMap<String, HistoryViewport>,
    ) -> Vec<TerminalFrame> {
        let Some(workspace) = self
            .workspaces
            .get(self.active_ws)
            .filter(|w| w.remote.is_none())
        else {
            return Vec::new();
        };
        let Some(tab) = workspace
            .tabs
            .get(workspace.active_tab)
            .filter(|t| t.is_renameable())
        else {
            return Vec::new();
        };
        tab.layout
            .leaves()
            .into_iter()
            .filter(|id| !self.zoomed || *id == tab.layout.focus)
            .filter_map(|id| {
                if self.views.contains_key(&id) {
                    return None;
                }
                let pane = self.panes.get(&id)?;
                let runtime = pane.terminal_runtime()?;
                let target = Target {
                    generation: self.backend_server_generation.clone(),
                    workspace_id: workspace.id.clone(),
                    tab_id: tab.id.clone(),
                    pane_id: id.0.to_string(),
                    terminal_id: Some(runtime.terminal_id),
                };
                let viewport = offsets.get_mut(target.terminal_id.as_ref()?);
                terminal_frame(pane, target, viewport)
            })
            .collect()
    }

    pub(crate) fn validate_gui_target(
        &self,
        target: &Target,
        require_terminal: bool,
    ) -> Result<(usize, usize, PaneId), String> {
        if target.generation != self.backend_server_generation {
            return Err("stale server generation".into());
        }
        let wi = self
            .workspaces
            .iter()
            .position(|ws| ws.id == target.workspace_id)
            .ok_or("workspace no longer exists")?;
        let workspace = &self.workspaces[wi];
        if workspace.remote.is_some() {
            return Err("remote GUI targets are unavailable".into());
        }
        let ti = workspace
            .tabs
            .iter()
            .position(|tab| tab.id == target.tab_id)
            .ok_or("tab no longer belongs to workspace")?;
        let tab = &workspace.tabs[ti];
        let id = PaneId(
            target
                .pane_id
                .parse::<u32>()
                .map_err(|_| "invalid pane id")?,
        );
        if target.pane_id != id.0.to_string() || !tab.is_renameable() || !tab.layout.contains(id) {
            return Err("pane no longer belongs to tab".into());
        }
        let view = self.views.get(&id);
        if matches!(view, Some(ViewKind::Remote(_))) {
            return Err("remote GUI targets are unavailable".into());
        }
        match (self.panes.get(&id), view) {
            (Some(pane), None) => {
                let runtime = pane
                    .terminal_runtime()
                    .ok_or("terminal runtime is not ready")?;
                if target.terminal_id.as_deref() != Some(runtime.terminal_id.as_str()) {
                    return Err("stale terminal runtime".into());
                }
            }
            (None, Some(_)) if !require_terminal && target.terminal_id.is_none() => {}
            _ => return Err("target is not an available local leaf".into()),
        }
        Ok((wi, ti, id))
    }

    fn gui_docks(&self, b: &mut Budget) -> Value {
        let sides = [Side::Left, Side::Right].map(|side| {
            let state = self.sidebars.get(side);
            let docks = state.docks.iter().map(|dock| {
                let data = match dock {
                    DockKind::Workspaces => json!({"source":"workspaces"}),
                    DockKind::Agents => json!({"source":"workspaces", "active_only":self.agents_active_only, "this_workspace":self.agents_this_workspace}),
                    DockKind::Files => {
                        let owner = self.workspaces.get(self.active_ws).filter(|ws| ws.remote.is_none());
                        if owner.is_none_or(|ws| !crate::platform::same_path(&ws.cwd, self.file_tree.root())) {
                            json!({"state":"unavailable", "reason":"no_matching_local_file_tree"})
                        } else {
                            let rows = self.file_tree.filter.as_ref().map(|filter| json!({
                                "query":b.text(&filter.query), "loading":filter.loading, "partial":filter.partial,
                                "generation":filter.generation.to_string(),
                                "rows":b.page(&filter.rows, self.file_tree.scroll, |row, b| json!({
                                    "path":row.path.to_string_lossy(), "name":b.text(&row.name),
                                    "depth":row.depth, "is_dir":row.is_dir, "expanded":row.expanded, "loading":row.loading,
                                })),
                            })).unwrap_or_else(|| json!({"state":"unavailable", "reason":"no_immutable_cached_row_access"}));
                            json!({
                                "root":self.file_tree.root().to_string_lossy(), "loaded":self.file_tree.root_loaded(),
                                "cursor":self.file_tree.cursor, "scroll":self.file_tree.scroll, "mode":self.files_mode,
                                "listing":rows,
                            })
                        }
                    },
                    DockKind::Module(id) => self.module_docks.get(id).map(|module| {
                        b.page(&module.rows, 0, |row, b| {
                            let value = row.value.as_deref().and_then(|value| b.exact(value));
                            let text = b.text(&row.text);
                            let action = row.action.as_ref().filter(|_| value == row.value && text == row.text);
                            json!({
                                "text":text, "dot":row.dot, "tone":row.tone, "action":action, "value":value,
                                "spans":b.page(&row.spans, 0, |span, b| json!({"text":b.text(&span.text), "tone":span.tone})),
                                "menu":b.page(&row.menu, 0, |item, b| {
                                    let payload = item.value.as_ref().or(row.value.as_ref());
                                    let value = payload.and_then(|value| b.exact(value));
                                    json!({
                                        "title":b.text(&item.title), "action":(value.as_ref() == payload && text == row.text).then_some(&item.action),
                                        "value":value, "destructive":item.destructive,
                                    })
                                }),
                            })
                        })
                    }).unwrap_or_else(|| json!({"state":"unavailable", "reason":"not_cached"})),
                };
                json!({"id":dock.id(), "title":self.dock_label(dock), "owner":if let DockKind::Module(id)=dock {self.module_owning_dock(id)}else{None}, "data":data})
            }).collect::<Vec<_>>();
            json!({"side":side, "visible":state.visible, "width":state.width, "weights":state.dock_weights(), "docks":docks})
        });
        json!(sides)
    }
}

fn terminal_frame(
    pane: &crate::terminal::pty::Pane,
    target: Target,
    viewport: Option<&mut HistoryViewport>,
) -> Option<TerminalFrame> {
    let (cols, rows) = pane.size();
    if !(1..=MAX_COLS).contains(&cols) || !(1..=MAX_ROWS).contains(&rows) {
        return None;
    }
    let engine = pane.engine.try_lock().ok()?;
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    let mut text_bytes = 0usize;
    let mut overflow = false;
    let scroll_offset = viewport.map_or(0, |viewport| viewport.resolve(&*engine));
    let start = engine.history_len().saturating_sub(scroll_offset);
    let mut markers = Vec::new();
    let wraps = engine.retained_styled_rows(
        start,
        rows,
        cols,
        &mut |row, col, text, style, link, width| {
            if row >= rows || col >= cols || overflow {
                return;
            }
            text_bytes = text_bytes
                .saturating_add(text.len())
                .saturating_add(link.map_or(0, str::len));
            if text_bytes > MAX_FRAME_TEXT_BYTES
                || cells.len() >= usize::from(cols) * usize::from(rows)
            {
                overflow = true;
                return;
            }
            let marker = text.starts_with(crate::terminal::graphics::MARKER);
            if marker {
                markers.push((row, col, text.to_owned(), style.fg, style.underline_color));
            }
            cells.push(Cell {
                row,
                col,
                text: if marker { " ".into() } else { text.into() },
                width,
                fg: pack(style.fg),
                bg: pack(style.bg),
                mods: style.mods.bits(),
                link: link.map(str::to_string),
            });
        },
    );
    let cursor = engine.live_cursor();
    let placements = engine
        .graphics()
        .map(|(namespace, p)| (namespace.to_owned(), p.to_vec()));
    let mut frame = TerminalFrame {
        target,
        revision: format!(
            "{}:{}:{}:{scroll_offset}:{cols}:{rows}",
            pane.content_revision(),
            engine.output_generation(),
            engine.geometry_revision()
        ),
        cols,
        rows,
        cells,
        cursor: (cursor.x < cols && cursor.y < rows).then_some(Cursor {
            row: cursor.y,
            col: cursor.x,
            visible: cursor.visible && scroll_offset == 0,
            shape: engine.cursor_shape().into(),
        }),
        history_rows: engine.history_len(),
        scroll_offset,
        geometry: engine.geometry_revision().to_string(),
        wraps,
        graphics: Vec::new(),
        modes: crate::ipc::app_surface::protocol::TerminalModes {
            mouse: engine.mouse_report(),
            mouse_drag: engine.mouse_drag(),
            mouse_motion: engine.mouse_motion(),
            alternate_screen: engine.alt_screen(),
            alternate_scroll: engine.alternate_scroll(),
            focus_reporting: engine.focus_reporting(),
        },
    };
    drop(engine);
    if !overflow {
        if let Some((namespace, placements)) = placements {
            use crate::ipc::app_surface::protocol::GraphicTiles;
            use crate::terminal::graphics;
            let area = ratatui::layout::Rect::new(0, 0, cols, rows);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            let mut scene = Vec::new();
            for (row, col, symbol, fg, underline) in markers {
                if let Some((marker, color)) = graphics::virtual_cell(
                    &symbol,
                    fg,
                    underline,
                    &namespace,
                    &placements,
                    &mut scene,
                ) {
                    buf[(col, row)].set_symbol(&marker).set_fg(color);
                }
            }
            if scroll_offset == 0 {
                graphics::draw(&mut buf, area, &namespace, &placements, &mut scene);
            }
            let mut layers: Vec<_> = scene
                .iter()
                .map(|g| GraphicTiles {
                    sequence: g.image.sequence.to_string(),
                    cols: g.size[0],
                    rows: g.size[1],
                    cells: vec![],
                })
                .collect();
            for (offset, cell) in buf.content.iter().enumerate() {
                let Some(layer) =
                    graphics::index(cell.symbol(), cell.fg).and_then(|i| layers.get_mut(i))
                else {
                    continue;
                };
                let Some([x, y]) = graphics::tile_of(cell.symbol()) else {
                    continue;
                };
                layer.cells.push([
                    (offset % usize::from(cols)) as u16,
                    (offset / usize::from(cols)) as u16,
                    x,
                    y,
                ]);
            }
            layers.retain(|layer| !layer.cells.is_empty());
            frame.graphics = layers;
        }
    }
    (!overflow).then_some(frame)
}

impl App {
    pub(crate) fn gui_graphic(
        &self,
        target: &Target,
        workspace: usize,
        sequence: &str,
    ) -> Result<Value, &'static str> {
        let (wi, ti, id) = self
            .validate_gui_target(target, true)
            .map_err(|_| "stale_target")?;
        if wi != workspace
            || self.workspaces[wi].active_tab != ti
            || (self.zoomed
                && wi == self.active_ws
                && self.workspaces[wi].tabs[ti].layout.focus != id)
        {
            return Err("stale_target");
        }
        let sequence = sequence.parse::<u64>().map_err(|_| "invalid_request")?;
        let image = {
            let engine = self.panes[&id]
                .engine
                .try_lock()
                .map_err(|_| "unavailable")?;
            engine
                .graphics()
                .and_then(|(_, placements)| {
                    placements.iter().find(|p| p.image.sequence == sequence)
                })
                .map(|p| p.image.clone())
                .ok_or("stale_image")?
        };
        if image.data.len() > 4 * 1024 * 1024
            || u64::from(image.width) * u64::from(image.height) > 2 * 1024 * 1024
        {
            return Err("limit_exceeded");
        }
        Ok(
            json!({"sequence":sequence.to_string(),"width":image.width,"height":image.height,"format":image.format,"compressed":image.compressed,"data":image.data}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_host_rows_expose_discovered_session_names_without_endpoint_details() {
        use crate::session::remote::{HostSession, HostStatus};
        let _env = crate::persist::test_env("gui-host-session-rows");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.remote_host_status = vec![
            HostStatus {
                host: "fixture-host".into(),
                sessions: vec![
                    HostSession {
                        name: "default".into(),
                        running: true,
                    },
                    HostSession {
                        name: "saved".into(),
                        running: false,
                    },
                ],
                error: None,
            },
            HostStatus {
                host: "fixture-offline".into(),
                sessions: vec![],
                error: Some("offline".into()),
            },
        ];
        let snapshot = app.gui_snapshot();
        assert_eq!(
            snapshot["host_connections"],
            json!([
                {"host":"fixture-host","state":"connected","sessions":["default","saved"]},
                {"host":"fixture-offline","state":"disconnected","sessions":[]},
            ])
        );
    }

    #[test]
    fn gui_target_fences_identity_and_native_leaf_ownership() {
        let _env = crate::persist::test_env("gui-projection-targets");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let id = app.workspaces[0].tabs[0].layout.focus;
        let target = Target {
            generation: app.backend_server_generation.clone(),
            workspace_id: app.workspaces[0].id.clone(),
            tab_id: app.workspaces[0].tabs[0].id.clone(),
            pane_id: id.0.to_string(),
            terminal_id: Some(app.panes[&id].terminal_runtime().unwrap().terminal_id),
        };
        assert_eq!(app.validate_gui_target(&target, true).unwrap(), (0, 0, id));
        let (response, _response_rx) = std::sync::mpsc::channel();
        let (cols, rows) = app.panes[&id].size();
        let engine = crate::terminal::vt::create_engine(
            Default::default(),
            cols,
            rows,
            response,
            64 * 1024,
            Default::default(),
        );
        engine.lock().unwrap().advance(
            "\x1b[31m\x1b]8;;https://example.com\x1b\\界e\u{301}\x1b]8;;\x1b\\".as_bytes(),
        );
        app.panes
            .get_mut(&id)
            .unwrap()
            .replace_engine_for_test(engine.clone());
        let frames = app.gui_terminal_frames(&mut Default::default());
        assert_eq!(frames[0].cells[0].text, "界");
        assert!(frames[0].cells.iter().any(|cell| cell.text == "e\u{301}"));
        assert_eq!(frames[0].cells[0].fg, pack(ratatui::style::Color::Red));
        assert_eq!(
            frames[0].cells[0].link.as_deref(),
            Some("https://example.com")
        );
        assert_eq!(frames[0].target.terminal_id, target.terminal_id);
        {
            let mut engine = engine.lock().unwrap();
            engine.advance("\r\nrow".repeat(40).as_bytes());
            engine.scroll(3);
        }
        let frames = app.gui_terminal_frames(&mut Default::default());
        assert_eq!(frames[0].scroll_offset, 0);
        assert_eq!(engine.lock().unwrap().scroll_offset(), 3);
        assert!(frames[0].cursor.as_ref().unwrap().visible);
        let mut viewport = HistoryViewport::default();
        viewport.scroll(&*engine.lock().unwrap(), 5);
        let mut offsets = [(target.terminal_id.clone().unwrap(), viewport)]
            .into_iter()
            .collect();
        let history = app.gui_terminal_frames(&mut offsets);
        assert_eq!(history[0].scroll_offset, 5);
        assert!(!history[0].cursor.as_ref().unwrap().visible);
        assert_eq!(engine.lock().unwrap().scroll_offset(), 3);
        for field in [
            "generation",
            "workspace_id",
            "tab_id",
            "pane_id",
            "terminal_id",
        ] {
            let mut stale = serde_json::to_value(&target).unwrap();
            stale[field] = json!("stale");
            let stale = serde_json::from_value(stale).unwrap();
            assert!(app.validate_gui_target(&stale, true).is_err());
        }
        let native = PaneId::alloc();
        app.workspaces[0].tabs[0]
            .layout
            .split_focused(crate::layout::Axis::Col, native);
        app.views.insert(
            native,
            ViewKind::File(crate::files::FileView::new("no-read.txt".into())),
        );
        let snapshot = app.gui_snapshot();
        let tab = &snapshot["workspaces"][0]["tabs"][0];
        assert_eq!(tab["id"], target.tab_id);
        assert_eq!(
            tab["layout"],
            json!(app.workspaces[0].tabs[0].layout.to_tree())
        );
        let leaf = &tab["panes"][1];
        assert_eq!(leaf["kind"], "file");
        assert_eq!(leaf["data"]["state"], "loading");
        let native_target: Target = serde_json::from_value(leaf["target"].clone()).unwrap();
        assert_eq!(
            app.validate_gui_target(&native_target, false).unwrap(),
            (0, 0, native)
        );
        assert!(app.validate_gui_target(&native_target, true).is_err());
        app.workspaces[0].tabs[0].layout.remove(native);
        assert!(app.validate_gui_target(&native_target, false).is_err());
        app.workspaces[0].remote = Some(super::super::remote::RemoteWorkspaceRef {
            host: "elsewhere".into(),
            session: "default".into(),
            workspace_id: "owner-workspace".into(),
        });
        assert!(app.validate_gui_target(&target, false).is_err());
        assert!(app.gui_terminal_frames(&mut Default::default()).is_empty());
        let snapshot = app.gui_snapshot();
        let remote = &snapshot["workspaces"][0];
        assert_eq!(remote["gui_available"], false);
        assert_eq!(remote["tabs"], json!([]));
        app.workspaces.clear();
        assert!(app.gui_terminal_frames(&mut Default::default()).is_empty());
        assert_eq!(app.gui_snapshot()["workspaces"], json!([]));
    }
}
