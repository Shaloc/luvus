use serde_json::{json, Value};

use crate::app::ViewKind;
use crate::files::preview::{Block, PreviewLoad};

pub(super) const ROW_LIMIT: usize = 256;
const TEXT_LIMIT: usize = 4096;

pub(super) struct Budget {
    pub bytes: usize,
    pub items: usize,
    cuts: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            bytes: 4 * 1024 * 1024,
            items: 16_384,
            cuts: 0,
        }
    }
}

impl Budget {
    pub fn text(&mut self, text: &str) -> String {
        let mut end = text.len().min(TEXT_LIMIT).min(self.bytes);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.bytes -= end;
        if end == 0 && !text.is_empty() {
            self.bytes = 0;
        }
        self.cuts += usize::from(end < text.len());
        text[..end].to_string()
    }

    pub fn exact(&mut self, text: &str) -> Option<String> {
        if text.len() > TEXT_LIMIT.min(self.bytes) {
            self.cuts += 1;
            return None;
        }
        self.bytes -= text.len();
        Some(text.to_string())
    }

    pub fn page<T>(
        &mut self,
        rows: &[T],
        offset: usize,
        mut project: impl FnMut(&T, &mut Self) -> Value,
    ) -> Value {
        let start = offset.min(rows.len());
        let cuts = self.cuts;
        let mut items = Vec::new();
        for row in rows.iter().skip(start).take(ROW_LIMIT) {
            if self.items == 0 || self.bytes == 0 {
                break;
            }
            self.items -= 1;
            items.push(project(row, self));
        }
        let end = start + items.len();
        json!({
            "items": items, "offset": start, "total": rows.len(),
            "next_offset": (end < rows.len()).then_some(end),
            "truncated": start > 0 || end < rows.len() || cuts != self.cuts,
        })
    }
}

pub(super) fn unavailable(kind: &str, title: &str, reason: &str) -> Value {
    json!({"kind":kind, "title":title, "data":{"state":"unavailable", "reason":reason}})
}

pub(super) fn native(view: &ViewKind, budget: &mut Budget) -> Value {
    let mut b = Budget {
        bytes: budget.bytes.min(128 * 1024),
        items: budget.items.min(1024),
        cuts: 0,
    };
    let (bytes, items) = (b.bytes, b.items);
    let (kind, title, mut data) = match view {
        ViewKind::File(view) => {
            let title = b.text(&view.path.to_string_lossy());
            let content = match &view.load {
                crate::files::FileLoad::Loading => json!({"state":"loading"}),
                crate::files::FileLoad::Text(lines) => json!({
                    "state":"text", "rows": b.page(lines, view.scroll, |line, b| json!({
                        "text": b.text(line), "source_bytes":line.len(),
                    })),
                }),
                crate::files::FileLoad::Binary(size) => json!({"state":"binary", "bytes":size}),
                crate::files::FileLoad::TooLarge(size) => {
                    json!({"state":"too_large", "bytes":size})
                }
                crate::files::FileLoad::Error(error) => {
                    json!({"state":"error", "error":b.text(error)})
                }
            };
            let mut data = content;
            data["path"] = json!(view.path.to_string_lossy());
            data["read_token"] = json!(view.read_token.to_string());
            data["scroll"] = json!(view.scroll);
            data["horizontal_scroll"] = json!(view.hscroll);
            data["wrap"] = json!(view.wrap);
            ("file", title, data)
        }
        ViewKind::Diff(view) => {
            let title = b.text(view.key.display_path());
            let mut data = match &view.load {
                crate::diff::DiffLoad::Loading => json!({"state":"loading"}),
                crate::diff::DiffLoad::Conflict(message) => {
                    json!({"state":"conflict", "message":b.text(message)})
                }
                crate::diff::DiffLoad::Error(error) => {
                    json!({"state":"error", "error":b.text(error)})
                }
                crate::diff::DiffLoad::Ready(diff)
                    if view.preference == crate::diff::DiffLayoutPreference::Split =>
                {
                    json!({
                        "state":"ready","binary":diff.binary,"additions":diff.additions,"deletions":diff.deletions,
                        "source_truncated":diff.truncated,"omitted_lines":diff.omitted_lines,
                        "split_rows":b.page(&view.split_rows,0,|row,b|{
                            let mut side=|line:&crate::diff::DiffLine|json!({"old_line":line.old_line,"new_line":line.new_line,"kind":line.kind,"text":b.text(&line.text)});
                            json!({"old":row.old.as_ref().map(&mut side),"new":row.new.as_ref().map(side)})
                        })
                    })
                }
                crate::diff::DiffLoad::Ready(diff) => json!({
                    "state":"ready", "status":diff.status, "binary":diff.binary,
                    "additions":diff.additions, "deletions":diff.deletions,
                    "source_truncated":diff.truncated, "omitted_lines":diff.omitted_lines,
                    "hunks":b.page(&diff.hunks, 0, |hunk, b| json!({
                        "id":hunk.id, "old_start":hunk.old_start, "new_start":hunk.new_start,
                        "header":b.text(&hunk.header),
                        "lines":b.page(&hunk.lines, 0, |line, b| json!({
                            "kind":line.kind, "old_line":line.old_line, "new_line":line.new_line,
                            "text":b.text(&line.text), "source_bytes":line.text.len(),
                        })),
                    })),
                }),
            };
            data["key"] = json!(view.key);
            data["root"] = json!(view.root.to_string_lossy());
            data["request_token"] = json!(view.request_token.to_string());
            data["layout"] = json!(view.preference);
            data["scroll"] = json!(view.scroll);
            data["selected"] = json!(view.selected);
            data["selected_side"] = json!(view.selected_side);
            data["horizontal_scroll"] = json!(view.horizontal);
            data["wrap"] = json!(view.wrap);
            data["context_lines"] = json!(view.context_lines);
            data["show_line_numbers"] = json!(view.show_line_numbers);
            data["dirty"] = json!(view.dirty);
            ("diff", title, data)
        }
        ViewKind::Preview(view) => {
            let title = b.text(&view.path.to_string_lossy());
            let mut data = match &view.load {
                PreviewLoad::Loading => json!({"state":"loading"}),
                PreviewLoad::Binary(size) => json!({"state":"binary", "bytes":size}),
                PreviewLoad::TooLarge(size) => json!({"state":"too_large", "bytes":size}),
                PreviewLoad::Error(error) => json!({"state":"error", "error":b.text(error)}),
                PreviewLoad::Ready(document) => json!({
                    "state":"ready", "content_format":"semantic_text",
                    "blocks":b.page(&document.blocks, 0, |block, b| block_value(block, &document.source, b)),
                }),
            };
            data["path"] = json!(view.path.to_string_lossy());
            data["preview_kind"] = json!(view.kind);
            data["read_token"] = json!(view.read_token.to_string());
            data["scroll"] = json!(view.scroll);
            ("preview", title, data)
        }
        ViewKind::Browser(url) => (
            "browser",
            "Browser".into(),
            json!({
                "state":"metadata_only", "url":b.exact(url), "source_bytes":url.len(),
            }),
        ),
        ViewKind::Remote(view) => (
            "remote",
            "Remote workspace".into(),
            json!({
                "state":"unavailable", "reason":"remote_projection_not_supported",
                "host":view.target.host, "session":view.target.session,
                "workspace_id":view.target.workspace_id,
            }),
        ),
    };
    data["text_truncated"] = json!(b.cuts > 0);
    budget.bytes -= bytes - b.bytes;
    budget.items -= items - b.items;
    json!({"kind":kind, "title":title, "data":data})
}

fn block_value(block: &Block, source: &str, b: &mut Budget) -> Value {
    macro_rules! inlines {
        ($content:expr, $budget:ident) => {
            $budget.page($content, 0, |span, b| json!({
                "text":b.text(&span.text), "role":format!("{:?}", span.role).to_ascii_lowercase(),
                "link_target":span.link.as_ref().and_then(|link| b.exact(link)),
            }))
        };
    }
    let mut data = match block {
        Block::Heading { level, content, .. } => {
            json!({"kind":"heading", "level":level, "content":inlines!(content, b)})
        }
        Block::Paragraph { content, .. } => {
            json!({"kind":"paragraph", "content":inlines!(content, b)})
        }
        Block::ListItem {
            depth,
            marker,
            checked,
            content,
            ..
        } => json!({
            "kind":"list_item", "depth":depth, "marker":b.text(marker), "checked":checked, "content":inlines!(content, b),
        }),
        Block::Quote { depth, content, .. } => {
            json!({"kind":"quote", "depth":depth, "content":inlines!(content, b)})
        }
        Block::Rule { .. } => json!({"kind":"rule"}),
        Block::Code { language, text, .. } => json!({
            "kind":"code", "language":language.as_ref().map(|v| b.text(v)), "text":b.text(text), "source_bytes":text.len(),
        }),
        Block::Table { header, rows, .. } => json!({
            "kind":"table",
            "header":b.page(header, 0, |cell, b| inlines!(cell, b)),
            "rows":b.page(rows, 0, |row, b| b.page(row, 0, |cell, b| inlines!(cell, b))),
        }),
        Block::ImagePlaceholder { alt, target, .. } => json!({
            "kind":"image_placeholder", "alt":b.text(alt), "target":b.exact(target), "loaded":false,
        }),
        Block::SourceFallback {
            source, diagnostic, ..
        } => json!({
            "kind":"source_fallback", "text":b.text(source), "diagnostic":b.text(diagnostic),
        }),
        Block::Mermaid { diagram, range } => {
            use crate::files::preview::mermaid::MermaidDiagram;
            match diagram {
                MermaidDiagram::Flowchart(graph) => json!({
                    "kind":"mermaid", "diagram_kind":"flowchart",
                    "direction":format!("{:?}", graph.direction).to_ascii_lowercase(),
                    "nodes":b.page(&graph.nodes, 0, |node, b| json!({
                        "id":b.text(&node.id), "label":b.text(&node.label),
                        "shape":format!("{:?}", node.shape).to_ascii_lowercase(),
                    })),
                    "edges":b.page(&graph.edges, 0, |edge, b| json!({
                        "from":edge.from, "to":edge.to, "dotted":edge.dotted, "directed":edge.directed,
                        "label":edge.label.as_ref().map(|label| b.text(label)),
                    })),
                }),
                MermaidDiagram::Sequence(sequence) => json!({
                    "kind":"mermaid", "diagram_kind":"sequence", "representation":"source_fallback",
                    "text":b.text(source.get(range.clone()).unwrap_or_default()),
                    "participants":b.page(&sequence.participants, 0, |participant, b| json!({
                        "label":b.text(&participant.label), "actor":participant.actor,
                    })),
                    "events":{"state":"unavailable", "reason":"no_public_semantic_event_access", "total":sequence.events.len()},
                }),
            }
        }
    };
    data["source_range"] = json!({"start":block.range().start, "end":block.range().end});
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{FileLoad, FileView};

    #[test]
    fn gui_native_file_states_and_bounded_rows() {
        let mut view = FileView::new("missing-without-reading.txt".into());
        view.read_token = 91;
        for (load, expected) in [
            (FileLoad::Loading, "loading"),
            (FileLoad::Binary(8), "binary"),
            (FileLoad::TooLarge(99), "too_large"),
            (FileLoad::Error("denied".into()), "error"),
        ] {
            view.load = load;
            let wrapped = ViewKind::File(view);
            let value = native(&wrapped, &mut Budget::default());
            assert_eq!(value["kind"], "file");
            assert_eq!(value["data"]["state"], expected);
            assert_eq!(value["data"]["read_token"], "91");
            let ViewKind::File(original) = wrapped else {
                unreachable!()
            };
            view = original;
        }
        view.load = FileLoad::Text(vec!["界".repeat(TEXT_LIMIT); ROW_LIMIT + 2]);
        view.scroll = 1;
        let value = native(&ViewKind::File(view), &mut Budget::default());
        let rows = &value["data"]["rows"];
        assert_eq!(rows["offset"], 1);
        assert_eq!(rows["total"], ROW_LIMIT + 2);
        assert_eq!(rows["truncated"], true);
        assert!(rows["next_offset"].as_u64().unwrap() < (ROW_LIMIT + 2) as u64);
        assert!(rows["items"][0]["text"].as_str().unwrap().len() <= TEXT_LIMIT);
    }

    #[test]
    fn gui_native_urls_are_omitted_instead_of_retargeted_by_truncation() {
        let value = native(
            &ViewKind::Browser("x".repeat(TEXT_LIMIT + 1)),
            &mut Budget::default(),
        );
        assert!(value["data"]["url"].is_null());
        assert_eq!(value["data"]["text_truncated"], true);
    }

    #[test]
    fn gui_native_diff_and_preview_are_semantic_not_rendered() {
        use crate::diff::model::{DiffHunk, FileDiff};
        use crate::diff::{DiffFileStatus, DiffKey, DiffLayer, DiffLine, DiffLineKind};
        let key = DiffKey {
            repo_id: "repo".into(),
            worktree_id: "worktree".into(),
            layer: DiffLayer::Worktree,
            old_path: None,
            new_path: None,
        };
        let mut view = crate::diff::DiffView::new(
            "repo".into(),
            key.clone(),
            Default::default(),
            3,
            true,
            true,
        );
        view.load = crate::diff::DiffLoad::Ready(std::sync::Arc::new(FileDiff {
            key,
            status: DiffFileStatus::Modified,
            additions: 1,
            deletions: 0,
            binary: false,
            truncated: false,
            omitted_lines: 0,
            hunks: vec![DiffHunk {
                id: "hunk".into(),
                old_start: 1,
                new_start: 1,
                header: "@@ -1 +1 @@".into(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(1),
                    text: "new".into(),
                }],
            }],
        }));
        let value = native(&ViewKind::Diff(Box::new(view)), &mut Budget::default());
        assert_eq!(
            value["data"]["hunks"]["items"][0]["lines"]["items"][0]["kind"],
            "addition"
        );
        let source = "<script>not executed</script>";
        let mut view = crate::files::preview::DocumentView::new(
            "missing.md".into(),
            crate::files::preview::PreviewKind::Markdown,
        );
        view.load = PreviewLoad::Ready(std::sync::Arc::new(
            crate::files::preview::PreviewDocument::new(
                source.into(),
                vec![Block::Code {
                    language: None,
                    text: source.into(),
                    range: 0..source.len(),
                }],
            ),
        ));
        let value = native(&ViewKind::Preview(view), &mut Budget::default());
        assert_eq!(value["data"]["content_format"], "semantic_text");
        assert_eq!(value["data"]["blocks"]["items"][0]["text"], source);
        assert!(value["data"].get("html").is_none());
        let value = native(
            &ViewKind::Browser("https://example.com".into()),
            &mut Budget::default(),
        );
        assert_eq!(value["data"]["state"], "metadata_only");
        assert_eq!(value["data"]["url"], "https://example.com");
    }
}
