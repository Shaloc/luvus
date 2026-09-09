//! Bounded Kitty direct-transfer images and screen-relative placements.
//!
//! This layer never opens files, shared memory or network resources. Applications
//! querying those media receive ENOTSUP and can negotiate direct transfer, which
//! is portable across a multiplexer's local and remote owners.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use base64::Engine;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_IMAGES: usize = 32;
const MAX_PLACEMENTS: usize = 128;
/// Unicode placeholder row/column diacritic table capacity.
pub const MAX_PLACEMENT_CELLS: u32 = 297;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Image {
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub compressed: bool,
    pub data: String,
}

impl Image {
    /// Validate newly received image data before forwarding it to a terminal.
    /// Cached immutable images do not need to be validated again.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_payload(
            &self.data,
            self.format,
            self.compressed,
            Some((self.width, self.height)),
        )
        .map(|_| ())
    }
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), &'static str> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("ENOSPC: invalid or excessive image dimensions");
    }
    Ok(())
}

/// Owner PNG uploads derive their dimensions from IHDR. Peer images must also
/// prove that those dimensions match their advertised metadata. Raw images
/// always require explicit dimensions. No image data is decompressed here.
fn validate_payload(
    payload: &str,
    format: u32,
    compressed: bool,
    dimensions: Option<(u32, u32)>,
) -> Result<(u32, u32), &'static str> {
    // Check all caller-supplied bounds before allocating the decoded buffer.
    if payload.len() > MAX_BYTES {
        return Err("ENOSPC: image exceeds byte limit");
    }
    if !matches!(format, 24 | 32 | 100) {
        return Err("ENOTSUP: image format");
    }
    if format == 100 && compressed {
        return Err("ENOTSUP: compressed PNG");
    }
    if let Some((width, height)) = dimensions {
        validate_dimensions(width, height)?;
    } else if format != 100 {
        return Err("EINVAL: missing image dimensions");
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| "EINVAL: invalid base64")?;
    let (width, height) = if format == 100 {
        if decoded.len() < 24
            || &decoded[..8] != b"\x89PNG\r\n\x1a\n"
            || &decoded[12..16] != b"IHDR"
        {
            return Err("EINVAL: invalid PNG header");
        }
        let actual = (
            u32::from_be_bytes(decoded[16..20].try_into().unwrap()),
            u32::from_be_bytes(decoded[20..24].try_into().unwrap()),
        );
        validate_dimensions(actual.0, actual.1)?;
        if dimensions.is_some_and(|expected| expected != actual) {
            return Err("EINVAL: PNG dimensions do not match metadata");
        }
        actual
    } else {
        // The explicit-dimensions guard above excludes None for raw images.
        dimensions.ok_or("EINVAL: missing image dimensions")?
    };
    if format != 100
        && !compressed
        && decoded.len() as u64 != u64::from(width) * u64::from(height) * u64::from(format / 8)
    {
        return Err("EINVAL: pixel byte count does not match dimensions");
    }
    Ok((width, height))
}

#[derive(Debug, Clone)]
pub struct Placement {
    /// Virtual resources are positioned only by child Unicode placeholders.
    pub virtual_placement: bool,
    pub image: Arc<Image>,
    pub id: u32,
    pub image_id: u32,
    pub row: i32,
    pub col: u32,
    pub rows: u32,
    pub cols: u32,
    /// Retained visible rows in screen coordinates, with an exclusive bottom.
    /// Keep the original origin/size so clipping never changes source sampling.
    pub clip_top: i32,
    pub clip_bottom: i32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub struct Graphics {
    images: VecDeque<(u32, Arc<Image>)>,
    placements: [Vec<Placement>; 2],
    alternate: usize,
    transfer: Option<(BTreeMap<u8, String>, String)>,
    next_image: u32,
    sequence: u64,
    generation: u64,
    bytes: usize,
}

type Params = BTreeMap<u8, String>;

fn number(params: &Params, key: u8, default: u32) -> Result<u32, &'static str> {
    params.get(&key).map_or(Ok(default), |value| {
        value.parse().map_err(|_| "EINVAL: invalid number")
    })
}

fn option(params: &Params, key: u8, default: &str) -> bool {
    params.get(&key).map_or(true, |value| value == default)
}

impl Graphics {
    /// Completed mutating commands, excluding queries and transfer fragments.
    /// Conservative: a successful delete of a missing image still counts.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn visible(&self) -> &[Placement] {
        &self.placements[self.alternate]
    }

    pub fn swap_alt(&mut self, entering: bool) {
        self.placements[1].clear();
        self.alternate = usize::from(entering);
        self.transfer = None;
    }

    pub fn clear(&mut self) {
        self.placements[self.alternate].clear();
        self.transfer = None;
    }

    pub fn erase_screen(&mut self) {
        // Erasing text removes placeholder cells, not their reusable virtual
        // resources. Applications redraw/crop those cells without re-upload.
        self.placements[self.alternate].retain(|p| p.virtual_placement);
        self.transfer = None;
    }

    pub fn reset(&mut self) {
        let sequence = self.sequence;
        let generation = self.generation.wrapping_add(1);
        *self = Self::default();
        self.sequence = sequence;
        self.generation = generation;
    }

    pub fn scroll(&mut self, start: i32, end: i32, delta: i32) {
        if start >= end || delta == 0 {
            return;
        }
        self.placements[self.alternate].retain_mut(|placement| {
            if placement.virtual_placement { return true; }
            let top = placement.row.max(placement.clip_top);
            let bottom = placement
                .row
                .saturating_add(placement.rows as i32)
                .min(placement.clip_bottom);
            // Page scrolling leaves overlapping and outside images untouched.
            // Once clipped, only the retained visible portion participates.
            if top >= start && bottom <= end {
                placement.row = placement.row.saturating_add(delta);
                placement.clip_top = top.saturating_add(delta).max(start);
                placement.clip_bottom = bottom.saturating_add(delta).min(end);
                placement.clip_top < placement.clip_bottom
            } else {
                true
            }
        });
    }

    /// Returns a PTY response plus cursor advance for a successfully placed image.
    pub fn command(
        &mut self,
        data: &[u8],
        row: i32,
        col: u32,
        cell: (u16, u16),
    ) -> (Option<String>, Option<(u32, u32)>) {
        let Some(data) = data.strip_prefix(b"G") else {
            return (None, None);
        };
        let split = data
            .iter()
            .position(|byte| *byte == b';')
            .unwrap_or(data.len());
        let Ok(header) = std::str::from_utf8(&data[..split]) else {
            return (None, None);
        };
        let Ok(payload) = std::str::from_utf8(data.get(split + 1..).unwrap_or_default()) else {
            return (None, None);
        };
        let mut params = Params::new();
        for field in header.split(',').filter(|field| !field.is_empty()) {
            let bytes = field.as_bytes();
            if bytes.len() < 3
                || bytes[1] != b'='
                || !bytes[0].is_ascii_alphabetic()
                || !bytes[2..]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
                || params.insert(bytes[0], field[2..].into()).is_some()
            {
                self.transfer = None;
                return (None, None);
            }
        }
        if params.get(&b'a').is_some_and(|action| action == "d") {
            self.transfer = None;
        }
        if let Some((first, mut pending)) = self.transfer.take() {
            if params.keys().any(|key| !matches!(*key, b'm' | b'q')) {
                return (None, None);
            }
            if pending.len().saturating_add(payload.len()) > MAX_BYTES {
                return (
                    Self::reply(&first, "ENOSPC: image exceeds byte limit"),
                    None,
                );
            }
            pending.push_str(payload);
            let more = number(&params, b'm', 0).unwrap_or(0) == 1;
            if more {
                self.transfer = Some((first, pending));
                return (None, None);
            }
            return self.complete(first, pending, row, col, cell);
        }
        if number(&params, b'm', 0).unwrap_or(0) == 1 {
            self.transfer = Some((params, payload.into()));
            return (None, None);
        }
        self.complete(params, payload.into(), row, col, cell)
    }

    fn reply(params: &Params, result: &str) -> Option<String> {
        let id = number(params, b'i', 0).ok()?;
        let quiet = number(params, b'q', 0).unwrap_or(2);
        if id == 0 || quiet == 2 || (quiet == 1 && result == "OK") {
            return None;
        }
        let placement = number(params, b'p', 0).unwrap_or(0);
        let placement = if placement == 0 {
            String::new()
        } else {
            format!(",p={placement}")
        };
        Some(format!("\x1b_Gi={id}{placement};{result}\x1b\\"))
    }

    fn complete(
        &mut self,
        params: Params,
        payload: String,
        row: i32,
        col: u32,
        cell: (u16, u16),
    ) -> (Option<String>, Option<(u32, u32)>) {
        let result = self.apply(&params, payload, row, col, cell);
        if result.is_ok() && params.get(&b'a').is_none_or(|action| action != "q") {
            self.generation = self.generation.wrapping_add(1);
        }
        match result {
            Ok(advance) => (Self::reply(&params, "OK"), advance),
            Err(error) => (Self::reply(&params, error), None),
        }
    }

    fn apply(
        &mut self,
        params: &Params,
        payload: String,
        row: i32,
        col: u32,
        cell: (u16, u16),
    ) -> Result<Option<(u32, u32)>, &'static str> {
        let action = params.get(&b'a').map_or("t", String::as_str);
        if !matches!(action, "q" | "t" | "T" | "p" | "d") {
            return Err("ENOTSUP: image action");
        }
        if !option(params, b't', "d") {
            return Err("ENOTSUP: use direct transfer t=d");
        }
        let virtual_placement = number(params, b'U', 0)?;
        if virtual_placement > 1 || (virtual_placement == 1 && number(params, b'p', 0)? != 0) {
            return Err("ENOTSUP: virtual placement option");
        }
        for key in [b'P', b'Q', b'I', b'X', b'Y', b'S', b'O'] {
            if number(params, key, 0)? != 0 {
                return Err("ENOTSUP: image placement option");
            }
        }
        let mut id = number(params, b'i', 0)?;
        let preserve_cursor = number(params, b'C', 0)?;
        if preserve_cursor > 1 || !option(params, b'z', "0") {
            return Err("ENOTSUP: cursor or stacking option");
        }
        if action == "d" {
            let kind = params.get(&b'd').map_or("a", String::as_str);
            let placement = number(params, b'p', 0)?;
            match kind {
                "a" | "A" => self.erase_screen(),
                "i" | "I" => self.placements[self.alternate]
                    .retain(|p| p.image_id != id || (placement != 0 && p.id != placement)),
                _ => return Err("ENOTSUP: image deletion selector"),
            }
            if matches!(kind, "I" | "A") {
                // Capital deletion frees data only after its last placement.
                // In particular the alternate screen cannot delete a primary
                // screen image, nor one named placement its sibling.
                let placements = &self.placements;
                self.images.retain(|(key, image)| {
                    let remove = (kind == "A" || *key == id)
                        && !placements.iter().flatten().any(|p| p.image_id == *key);
                    if remove {
                        self.bytes -= image.data.len();
                    }
                    !remove
                });
            }
            return Ok(None);
        }
        let mut upload = None;
        if matches!(action, "t" | "T" | "q") {
            let format = number(params, b'f', 32)?;
            if !option(params, b'o', "z") {
                return Err("ENOTSUP: image format");
            }
            let dimensions = if format == 100 {
                None
            } else {
                Some((number(params, b's', 0)?, number(params, b'v', 0)?))
            };
            let compressed = params.contains_key(&b'o');
            let (width, height) = validate_payload(&payload, format, compressed, dimensions)?;
            if action == "q" {
                return Ok(None);
            }
            if id == 0 {
                loop {
                    self.next_image = self.next_image.wrapping_add(1).max(1);
                    if !self.images.iter().any(|(id, _)| *id == self.next_image) {
                        break;
                    }
                }
                id = self.next_image;
            }
            let image = Arc::new(Image {
                sequence: self.sequence.wrapping_add(1),
                width,
                height,
                format,
                compressed,
                data: payload,
            });
            if action == "t" {
                self.store_image(id, image)?;
                return Ok(None);
            }
            upload = Some(image);
        }
        let image = upload
            .clone()
            .or_else(|| {
                self.images
                    .iter()
                    .find(|(key, _)| *key == id)
                    .map(|(_, image)| Arc::clone(image))
            })
            .ok_or("ENOENT: image not found")?;
        let x = number(params, b'x', 0)?;
        let y = number(params, b'y', 0)?;
        let width = number(params, b'w', image.width.saturating_sub(x))?;
        let height = number(params, b'h', image.height.saturating_sub(y))?;
        if width == 0
            || height == 0
            || x.checked_add(width).is_none_or(|end| end > image.width)
            || y.checked_add(height).is_none_or(|end| end > image.height)
        {
            return Err("EINVAL: source rectangle");
        }
        // Kitty virtual placements ignore source crop options. Until the
        // compositor can crop pixels, do not acknowledge a different image.
        if x != 0 || y != 0 || width != image.width || height != image.height {
            return Err("ENOTSUP: source cropping");
        }
        let requested_cols = number(params, b'c', 0)?;
        let requested_rows = number(params, b'r', 0)?;
        if requested_cols > 4096 || requested_rows > 4096 {
            return Err("EINVAL: placement dimensions");
        }
        let cw = u64::from(cell.0.max(1));
        let ch = u64::from(cell.1.max(1));
        let (cols, rows) = match (requested_cols, requested_rows) {
            (0, 0) => (
                u64::from(width).div_ceil(cw),
                u64::from(height).div_ceil(ch),
            ),
            (0, rows) => (
                (u64::from(rows) * ch * u64::from(width)).div_ceil(u64::from(height) * cw),
                u64::from(rows),
            ),
            (cols, 0) => (
                u64::from(cols),
                (u64::from(cols) * cw * u64::from(height)).div_ceil(u64::from(width) * ch),
            ),
            (cols, rows) => (u64::from(cols), u64::from(rows)),
        };
        if cols == 0 || rows == 0 || cols > 4096 || rows > 4096 {
            return Err("EINVAL: placement dimensions");
        }
        if cols > u64::from(MAX_PLACEMENT_CELLS) || rows > u64::from(MAX_PLACEMENT_CELLS) {
            return Err("ENOTSUP: placement exceeds Unicode placeholder range");
        }
        // Virtual placements preserve aspect ratio. Allow natural rounding to
        // a terminal cell, but reject explicitly stretched rectangles.
        if virtual_placement == 0 && requested_cols != 0 && requested_rows != 0
            && rows != (cols * cw * u64::from(height)).div_ceil(u64::from(width) * ch)
            && cols != (rows * ch * u64::from(width)).div_ceil(u64::from(height) * cw)
        {
            return Err("ENOTSUP: stretched placement");
        }
        let (cols, rows) = (cols as u32, rows as u32);
        let pid = number(params, b'p', 0)?;
        if let Some(image) = upload {
            self.store_image(id, image)?;
        }
        let placements = &mut self.placements[self.alternate];
        if pid != 0 {
            placements.retain(|p| p.image_id != id || p.id != pid);
        } else if virtual_placement == 1 {
            placements.retain(|p| p.image_id != id || p.id != 0 || !p.virtual_placement);
        }
        if placements.len() >= MAX_PLACEMENTS {
            placements.remove(0);
        }
        placements.push(Placement {
            virtual_placement: virtual_placement == 1,
            image,
            id: pid,
            image_id: id,
            row,
            col,
            rows,
            cols,
            clip_top: i32::MIN,
            clip_bottom: i32::MAX,
            x,
            y,
            width,
            height,
        });
        Ok((preserve_cursor != 1 && virtual_placement == 0).then_some((rows, cols)))
    }

    fn store_image(&mut self, id: u32, image: Arc<Image>) -> Result<(), &'static str> {
        self.remove_image(id);
        while self.bytes + image.data.len() > MAX_BYTES || self.images.len() >= MAX_IMAGES {
            let oldest = self.images.front().ok_or("ENOSPC: image storage")?.0;
            self.remove_image(oldest);
        }
        self.sequence = image.sequence;
        self.bytes += image.data.len();
        self.images.push_back((id, image));
        Ok(())
    }

    fn remove_image(&mut self, id: u32) {
        self.images.retain(|(key, image)| {
            if *key == id {
                self.bytes -= image.data.len();
                false
            } else {
                true
            }
        });
        for screen in &mut self.placements {
            screen.retain(|p| p.image_id != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, format: u32, data: String) -> Image {
        Image {
            sequence: 1,
            width,
            height,
            format,
            compressed: false,
            data,
        }
    }

    // Only the header is needed to test the boundary's dimension extraction;
    // full PNG decoding remains the display terminal's responsibility.
    fn png_header(width: u32, height: u32) -> String {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn graphics_with_image(width: u32, height: u32) -> Graphics {
        let mut graphics = Graphics::default();
        let command = format!("Ga=t,i=1,f=100;{}", png_header(width, height));
        let reply = graphics
            .command(command.as_bytes(), 0, 0, (8, 16))
            .0
            .unwrap();
        assert!(reply.contains(";OK"), "{reply}");
        graphics
    }

    #[test]
    fn single_axis_placement_preserves_source_aspect_and_cell_pixel_ratio() {
        for (options, expected) in [
            ("r=4", (16, 4)),
            ("c=20", (20, 5)),
            ("c=20,r=5", (20, 5)),
            ("", (10, 3)),
        ] {
            let mut graphics = graphics_with_image(80, 40);
            let command = format!("Ga=p,i=1,C=1,{options};");
            let reply = graphics
                .command(command.as_bytes(), 0, 0, (8, 16))
                .0
                .unwrap();
            assert!(reply.contains(";OK"), "{reply}");
            let placement = &graphics.visible()[0];
            assert_eq!((placement.cols, placement.rows), expected, "{options}");
        }
    }

    #[test]
    fn unsupported_virtual_geometry_is_rejected_without_replacing_image() {
        let mut graphics = graphics_with_image(80, 40);
        let prior = Arc::clone(&graphics.images[0].1);
        for options in ["x=40,w=40,r=4", "c=20,r=1", "c=298"] {
            let command = format!("Ga=T,i=1,f=100,{options};{}", png_header(80, 40));
            let reply = graphics.command(command.as_bytes(), 0, 0, (8, 16)).0.unwrap();
            assert!(reply.contains("ENOTSUP"), "{reply}");
            assert!(graphics.visible().is_empty());
            assert!(Arc::ptr_eq(&graphics.images[0].1, &prior));
        }
    }

    #[test]
    fn scroll_ignores_placements_partially_overlapping_page_margins() {
        for (row, rows) in [(3, 4), (10, 4), (2, 12)] {
            let mut graphics = graphics_with_image(1, 1);
            let command = format!("Ga=p,i=1,c={},r={rows},C=1;", rows * 2);
            graphics.command(command.as_bytes(), row, 0, (8, 16));
            graphics.scroll(5, 12, -1);
            assert_eq!(graphics.visible()[0].row, row);
        }
    }

    #[test]
    fn scroll_clipped_rows_do_not_return_after_reverse_scroll() {
        let mut graphics = graphics_with_image(1, 1);
        graphics.command(b"Ga=p,i=1,c=6,r=3,C=1;", 5, 0, (8, 16));
        graphics.scroll(5, 12, -1);
        assert_eq!(graphics.visible()[0].row, 4);
        assert_eq!(graphics.visible()[0].clip_top, 5);
        graphics.scroll(5, 12, 1);
        assert_eq!(graphics.visible()[0].row, 5);
        assert_eq!(graphics.visible()[0].clip_top, 6);
        // The original first row was clipped, so only rows [6, 8) remain.
        // Scrolling [5, 6) must not find any remaining image row to move.
        graphics.scroll(5, 6, -1);
        assert_eq!(graphics.visible()[0].row, 5);
    }

    #[test]
    fn scrolling_clips_bottom_permanently_and_discards_fully_clipped_placements() {
        let mut graphics = graphics_with_image(1, 1);
        graphics.command(b"Ga=p,i=1,c=6,r=3,C=1;", 10, 0, (8, 16));
        graphics.scroll(5, 13, 1);
        assert_eq!(graphics.visible()[0].row, 11);
        assert_eq!(graphics.visible()[0].clip_bottom, 13);
        graphics.scroll(5, 13, -1);
        let placement = &graphics.visible()[0];
        assert_eq!(
            (placement.row, placement.clip_top, placement.clip_bottom),
            (10, 10, 12)
        );
        assert_eq!((placement.rows, placement.y, placement.height), (3, 0, 1));
        graphics.scroll(5, 12, 2);
        assert!(graphics.visible().is_empty());
    }

    #[test]
    fn derived_placement_bounds_reject_before_mutating_image_or_placements() {
        let mut graphics = graphics_with_image(1, MAX_PIXELS as u32);
        let reply = graphics
            .command(b"Ga=p,i=1,c=4096,C=1;", 0, 0, (8, 16))
            .0
            .unwrap();
        assert!(reply.contains("EINVAL: placement dimensions"), "{reply}");
        assert!(graphics.visible().is_empty());
        assert_eq!(graphics.images.len(), 1);
        let prior = Arc::clone(&graphics.images[0].1);
        let replace = format!("Ga=T,i=1,f=100,c=4294967295;{}", png_header(1, 1));
        let reply = graphics
            .command(replace.as_bytes(), 0, 0, (8, 16))
            .0
            .unwrap();
        assert!(reply.contains("EINVAL: placement dimensions"), "{reply}");
        assert!(Arc::ptr_eq(&graphics.images[0].1, &prior));
        let replace = format!("Ga=T,i=1,f=100,x=1;{}", png_header(1, 1));
        let reply = graphics
            .command(replace.as_bytes(), 0, 0, (8, 16))
            .0
            .unwrap();
        assert!(reply.contains("EINVAL: source rectangle"), "{reply}");
        assert!(Arc::ptr_eq(&graphics.images[0].1, &prior));
    }

    #[test]
    fn image_validation_rejects_png_metadata_mismatch_and_hidden_pixel_excess() {
        let good = image(2, 3, 100, png_header(2, 3));
        assert!(good.validate().is_ok());
        let mismatch = image(1, 1, 100, good.data);
        assert_eq!(
            mismatch.validate(),
            Err("EINVAL: PNG dimensions do not match metadata")
        );
        let oversized = image(1, 1, 100, png_header(10_000, 10_000));
        assert_eq!(
            oversized.validate(),
            Err("ENOSPC: invalid or excessive image dimensions")
        );
        let invalid_header = image(1, 1, 100, "AAAA".into());
        assert_eq!(invalid_header.validate(), Err("EINVAL: invalid PNG header"));
    }

    #[test]
    fn image_validation_checks_bounds_before_decoding_and_raw_byte_counts() {
        let invalid_dimensions = image(u32::MAX, u32::MAX, 32, "!".into());
        assert_eq!(
            invalid_dimensions.validate(),
            Err("ENOSPC: invalid or excessive image dimensions")
        );
        let excessive_bytes = image(1, 1, 24, "!".repeat(MAX_BYTES + 1));
        assert_eq!(
            excessive_bytes.validate(),
            Err("ENOSPC: image exceeds byte limit")
        );
        let good = image(1, 1, 24, "/wAA".into());
        assert!(good.validate().is_ok());
        let missing_alpha = image(1, 1, 32, good.data);
        assert_eq!(
            missing_alpha.validate(),
            Err("EINVAL: pixel byte count does not match dimensions")
        );
        assert_eq!(
            image(1, 1, 24, "====".into()).validate(),
            Err("EINVAL: invalid base64")
        );
    }

    #[test]
    fn owner_and_peer_share_validation_without_inflating_compressed_pixels() {
        let data = png_header(2, 3);
        assert_eq!(validate_payload(&data, 100, false, None), Ok((2, 3)));
        let mut compressed_png = image(2, 3, 100, data);
        compressed_png.compressed = true;
        assert_eq!(compressed_png.validate(), Err("ENOTSUP: compressed PNG"));
        assert_eq!(
            validate_payload(&compressed_png.data, 100, true, None),
            Err("ENOTSUP: compressed PNG")
        );
        // Zlib bytes are validated as base64 and size-bounded, not inflated.
        // The Kitty display rejects an invalid stream or a wrong decoded size.
        let mut compressed_raw = image(1, 1, 32, "AA==".into());
        compressed_raw.compressed = true;
        assert!(compressed_raw.validate().is_ok());
    }
}
