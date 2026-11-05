use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
pub const TRANSPORT_VERSION: u32 = 21;
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PENDING: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Snapshot {
        id: String,
    },
    Action {
        id: String,
        generation: String,
        method: String,
        params: Value,
    },
    Input {
        id: String,
        target: Target,
        input: Input,
    },
    Viewport {
        id: String,
        target: Target,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
    },
    Focus {
        id: String,
        target: Target,
        #[serde(default)]
        activate: bool,
    },
    FocusHistory {
        id: String,
        target: Target,
        forward: bool,
    },
    Graphic {
        id: String,
        target: Target,
        sequence: String,
    },
    PullRequests {
        id: String,
        generation: String,
        workspace_id: String,
        number: Option<u32>,
    },
    NotificationInbox {
        id: String,
        generation: String,
        mark_read: bool,
    },
    Logs {
        id: String,
        generation: String,
        client: bool,
    },
    Widget {
        id: String,
        generation: String,
        workspace_id: String,
        item: WidgetRef,
    },
    FileEntry {
        id: String,
        generation: String,
        workspace_id: String,
        root: String,
        path: String,
        operation: FileOperation,
    },
    BrowserOpen {
        id: String,
        target: Target,
    },
    BrowserTab {
        id: String,
        target: Target,
        url: String,
    },
    MouseOptions {
        id: String,
        target: Target,
        right_click_to_app: bool,
        copy_on_select: bool,
    },
    BrowserTakeover {
        id: String,
        browser_id: String,
        #[serde(default)]
        lease_generation: Option<String>,
    },
    BrowserClosed {
        id: String,
        browser_id: String,
    },
    BrowserResult {
        id: String,
        target: Target,
        lease_generation: String,
        value: Value,
        error: Option<String>,
    },
    BrowserCdpEvent {
        id: String,
        browser_id: String,
        target: Target,
        lease_generation: String,
        chunk: CdpChunk,
    },
    Detach {
        id: String,
    },
    /// Native adapter sends this only after the OS clipboard write succeeds.
    ClipboardSucceeded {
        id: String,
        receipt: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WidgetRef {
    Dock {
        key: String,
        owner: String,
        index: u16,
        action: String,
        text: String,
        value: Option<String>,
        menu: Option<u16>,
    },
    Bar {
        key: String,
        owner: String,
        index: u16,
        action: String,
        value: Option<String>,
        compact: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileOperation {
    Expand,
    Collapse,
    Markdown,
    Mermaid,
    Inspect,
    CreateFile { name: String },
    CreateDirectory { name: String },
    Rename { name: String, identity: String },
    Delete { identity: String },
}

impl Request {
    pub fn id(&self) -> &str {
        match self {
            Self::Snapshot { id }
            | Self::Action { id, .. }
            | Self::Input { id, .. }
            | Self::Viewport { id, .. }
            | Self::Focus { id, .. }
            | Self::FocusHistory { id, .. }
            | Self::Graphic { id, .. }
            | Self::PullRequests { id, .. }
            | Self::NotificationInbox { id, .. }
            | Self::Logs { id, .. }
            | Self::Widget { id, .. }
            | Self::FileEntry { id, .. }
            | Self::BrowserOpen { id, .. }
            | Self::BrowserTab { id, .. }
            | Self::MouseOptions { id, .. }
            | Self::BrowserTakeover { id, .. }
            | Self::BrowserClosed { id, .. }
            | Self::BrowserResult { id, .. }
            | Self::BrowserCdpEvent { id, .. }
            | Self::Detach { id }
            | Self::ClipboardSucceeded { id, .. } => id,
        }
    }

    pub fn has_valid_id(&self) -> bool {
        let id = self.id();
        !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.has_valid_id() {
            return Err("invalid request id");
        }
        match self {
            Self::Action {
                generation,
                method,
                params,
                ..
            } => {
                if generation.is_empty()
                    || generation.len() > 128
                    || method.len() > 128
                    || !params.is_object()
                {
                    return Err("invalid action");
                }
            }
            Self::Input { target, input, .. } => {
                target.validate()?;
                match input {
                    Input::Paste { text } if text.len() > 256 * 1024 => {
                        return Err("paste is too large")
                    }
                    Input::Key { key, .. } if key.is_empty() || key.len() > 64 => {
                        return Err("invalid key")
                    }
                    Input::Mouse {
                        geometry,
                        row,
                        col,
                        button,
                        ..
                    } if !valid_geometry(geometry) || *row >= 300 || *col >= 500 || *button > 3 => {
                        return Err("invalid mouse input")
                    }
                    Input::Selection {
                        geometry,
                        revision,
                        start,
                        end,
                        ..
                    } if !valid_geometry(geometry)
                        || revision.is_empty()
                        || revision.len() > 128
                        || [start, end]
                            .iter()
                            .any(|point| point[0] >= 300 || point[1] >= 500) =>
                    {
                        return Err("invalid selection")
                    }
                    Input::Copy { geometry } if !valid_geometry(geometry) => {
                        return Err("invalid geometry revision")
                    }
                    _ => {}
                }
            }
            Self::NotificationInbox { generation, .. } | Self::Logs { generation, .. }
                if generation.is_empty()
                    || generation.len() > 128
                    || generation.chars().any(char::is_control) =>
            {
                return Err("invalid generation")
            }
            Self::ClipboardSucceeded { receipt, .. } if !valid_geometry(receipt) => {
                return Err("invalid clipboard receipt")
            }
            Self::Viewport {
                target,
                cols,
                rows,
                cell_width,
                cell_height,
                ..
            } => {
                target.validate()?;
                if !(2..=500).contains(cols)
                    || !(2..=300).contains(rows)
                    || !(1..=512).contains(cell_width)
                    || !(1..=512).contains(cell_height)
                {
                    return Err("invalid viewport");
                }
            }
            Self::Widget {
                generation,
                workspace_id,
                item,
                ..
            } => {
                let (key, owner, action, value) = match item {
                    WidgetRef::Dock {
                        key,
                        owner,
                        action,
                        text,
                        value,
                        ..
                    } => {
                        if text.len() > 8192 {
                            return Err("invalid widget row");
                        }
                        (key, owner, action, value)
                    }
                    WidgetRef::Bar {
                        key,
                        owner,
                        action,
                        value,
                        ..
                    } => (key, owner, action, value),
                };
                if [generation, workspace_id, key, owner, action]
                    .into_iter()
                    .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
                    || value.as_ref().is_some_and(|s| s.len() > 8192)
                {
                    return Err("invalid widget action");
                }
            }
            Self::PullRequests {
                generation,
                workspace_id,
                number,
                ..
            } => {
                if generation.is_empty()
                    || generation.len() > 128
                    || workspace_id.is_empty()
                    || workspace_id.len() > 128
                    || number == &Some(0)
                {
                    return Err("invalid repository route");
                }
            }
            Self::Graphic {
                target, sequence, ..
            } => {
                target.validate()?;
                sequence
                    .parse::<u64>()
                    .map_err(|_| "invalid image sequence")?;
            }
            Self::FileEntry {
                generation,
                workspace_id,
                root,
                path,
                operation,
                ..
            } => {
                if [generation, workspace_id]
                    .iter()
                    .any(|s| s.is_empty() || s.len() > 128)
                    || [root, path]
                        .iter()
                        .any(|s| s.is_empty() || s.len() > 8192 || s.chars().any(char::is_control))
                {
                    return Err("invalid workspace file route");
                }
                match operation {
                    FileOperation::Rename { name, .. }
                    | FileOperation::CreateFile { name }
                    | FileOperation::CreateDirectory { name }
                        if name.is_empty()
                            || name.len() > 255
                            || name == "."
                            || name == ".."
                            || name.eq_ignore_ascii_case(".git")
                            || name.contains(['/', '\\'])
                            || name.chars().any(char::is_control) =>
                    {
                        return Err("invalid file name")
                    }
                    FileOperation::Rename { identity, .. } | FileOperation::Delete { identity }
                        if identity.is_empty() || identity.len() > 128 =>
                    {
                        return Err("invalid file identity")
                    }
                    _ => {}
                }
            }
            Self::BrowserTab { target, url, .. } => {
                target.validate()?;
                if url != crate::browser::BLANK_URL && !crate::browser::valid_url(url) {
                    return Err("invalid browser URL");
                }
            }
            Self::MouseOptions { target, .. }
            | Self::Focus { target, .. }
            | Self::FocusHistory { target, .. }
            | Self::BrowserOpen { target, .. } => target.validate()?,
            Self::BrowserTakeover {
                lease_generation: Some(value),
                ..
            } if value.parse::<u64>().is_err() => return Err("invalid lease generation"),
            Self::BrowserTakeover { browser_id, .. } | Self::BrowserClosed { browser_id, .. } => {
                if !crate::terminal::backend::valid_id(browser_id) {
                    return Err("invalid browser id");
                }
            }
            Self::BrowserCdpEvent {
                browser_id,
                target,
                lease_generation,
                chunk,
                ..
            } => {
                target.validate()?;
                chunk.validate()?;
                if !crate::terminal::backend::valid_id(browser_id)
                    || lease_generation.parse::<u64>().is_err()
                {
                    return Err("invalid CDP route");
                }
            }
            Self::BrowserResult {
                target,
                lease_generation,
                error,
                ..
            } => {
                target.validate()?;
                if lease_generation.parse::<u64>().is_err()
                    || error.as_ref().is_some_and(|error| error.len() > 128)
                {
                    return Err("invalid browser completion");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CdpChunk {
    pub transfer: String,
    pub index: u32,
    pub total: u32,
    pub text: String,
}

impl CdpChunk {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::terminal::backend::valid_id(&self.transfer)
            || self.total == 0
            || self.total > 16_384
            || self.index >= self.total
            || self.text.len() > 128 * 1024
            || self.text.is_empty()
        {
            return Err("invalid CDP chunk");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub generation: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub terminal_id: Option<String>,
}

impl Target {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if [
            &self.generation,
            &self.workspace_id,
            &self.tab_id,
            &self.pane_id,
        ]
        .iter()
        .any(|value| value.is_empty() || value.len() > 128 || value.chars().any(char::is_control))
            || self
                .terminal_id
                .as_ref()
                .is_some_and(|value| !crate::terminal::backend::valid_id(value))
        {
            return Err("invalid target");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Paste {
        text: String,
    },
    Key {
        key: String,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        release: bool,
        #[serde(default)]
        repeat: bool,
    },
    Scroll {
        lines: i16,
    },
    Mouse {
        geometry: String,
        action: MouseAction,
        button: u8,
        row: u16,
        col: u16,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
    },
    Selection {
        geometry: String,
        revision: String,
        /// Viewport [row, column], inclusive, using engine cell coordinates.
        start: [u16; 2],
        end: [u16; 2],
        mode: SelectionMode,
    },
    Copy {
        geometry: String,
    },
    FocusEvent {
        focused: bool,
    },
}

fn valid_geometry(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|c| c.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    Press,
    Release,
    Move,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    Cell,
    Word,
    Line,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ready {
        protocol: u32,
        transport: u32,
        capabilities: Vec<String>,
    },
    Snapshot {
        snapshot: Value,
    },
    Terminal {
        frame: Box<TerminalFrame>,
    },
    Reply {
        id: String,
        result: Value,
    },
    Error {
        id: Option<String>,
        code: String,
        message: String,
    },
    Effect {
        kind: String,
        data: Value,
    },
    Chunk {
        transfer: String,
        index: u32,
        total: u32,
        text: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalFrame {
    pub target: Target,
    pub revision: String,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    pub cursor: Option<Cursor>,
    pub history_rows: usize,
    pub scroll_offset: usize,
    pub geometry: String,
    pub wraps: Vec<bool>,
    pub modes: TerminalModes,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphics: Vec<GraphicTiles>,
}

/// Cell-owned image tiles. Image bytes are fetched on demand exactly for
/// this terminal lifetime; a text frame does not retransmit its bitmaps.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GraphicTiles {
    pub sequence: String,
    pub cols: u32,
    pub rows: u32,
    /// Display column/row and the source tile column/row.
    pub cells: Vec<[u16; 4]>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalModes {
    pub mouse: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub alternate_screen: bool,
    pub alternate_scroll: bool,
    pub focus_reporting: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Cell {
    pub row: u16,
    pub col: u16,
    pub text: String,
    pub width: u8,
    pub fg: u32,
    pub bg: u32,
    pub mods: u16,
    pub link: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: String,
}

pub fn read<R: Read>(reader: &mut R) -> io::Result<Option<Request>> {
    let mut size = [0; 4];
    match reader.read(&mut size[..1])? {
        0 => return Ok(None),
        1 => reader.read_exact(&mut size[1..])?,
        _ => unreachable!(),
    }
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > MAX_ENVELOPE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid app envelope length",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    Ok(Some(request))
}

pub fn write<W: Write>(writer: &mut W, response: &Response) -> io::Result<()> {
    let bytes = serde_json::to_vec(response).map_err(io::Error::other)?;
    if bytes.len() > MAX_OBJECT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "app object exceeds transfer budget",
        ));
    }
    if bytes.len() <= MAX_ENVELOPE_BYTES {
        return write_bytes(writer, &bytes);
    }
    let text = String::from_utf8(bytes).map_err(io::Error::other)?;
    let transfer = crate::terminal::backend::random_id().map_err(io::Error::other)?;
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 128 * 1024).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    let total = chunks.len() as u32;
    for (index, text) in chunks.into_iter().enumerate() {
        let bytes = serde_json::to_vec(&Response::Chunk {
            transfer: transfer.clone(),
            index: index as u32,
            total,
            text: text.into(),
        })
        .map_err(io::Error::other)?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(io::Error::other("app chunk exceeds envelope budget"));
        }
        write_bytes(writer, &bytes)?;
    }
    Ok(())
}

fn write_bytes<W: Write>(writer: &mut W, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_surface_rejects_oversized_and_unknown_requests() {
        assert!(read(&mut ((MAX_ENVELOPE_BYTES as u32) + 1).to_le_bytes().as_slice()).is_err());
        let bytes = br#"{"type":"detach","id":"1","command":"shell"}"#;
        let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
        framed.extend(bytes);
        assert!(read(&mut framed.as_slice()).is_err());
    }

    #[test]
    fn app_surface_handles_partial_headers_and_clean_eof() {
        assert!(read(&mut [].as_slice()).unwrap().is_none());
        assert!(read(&mut [1, 0].as_slice()).is_err());
        let request = Request::Snapshot { id: "one".into() };
        let bytes = serde_json::to_vec(&request).unwrap();
        let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
        framed.extend(bytes);
        assert_eq!(read(&mut framed.as_slice()).unwrap().unwrap().id(), "one");
    }

    #[test]
    fn app_surface_chunks_unicode_without_splitting_scalars() {
        let response = Response::Reply {
            id: "one".into(),
            result: Value::String("界\\\"".repeat(300_000)),
        };
        let mut output = Vec::new();
        write(&mut output, &response).unwrap();
        let mut stream = output.as_slice();
        let mut text = String::new();
        let mut expected_index = 0;
        while !stream.is_empty() {
            let mut length = [0; 4];
            stream.read_exact(&mut length).unwrap();
            let length = u32::from_le_bytes(length) as usize;
            assert!(length <= MAX_ENVELOPE_BYTES);
            let chunk: Response = serde_json::from_slice(&stream[..length]).unwrap();
            stream = &stream[length..];
            let Response::Chunk {
                index, text: part, ..
            } = chunk
            else {
                panic!("expected chunk")
            };
            assert_eq!(index, expected_index);
            expected_index += 1;
            text.push_str(&part);
        }
        assert_eq!(text, serde_json::to_string(&response).unwrap());
    }
}
