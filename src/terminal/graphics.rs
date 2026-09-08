//! Graphic content follows the same composed cells as text. Kitty virtual
//! placements make pane clipping, overlapping menus and remote chrome ordinary
//! buffer composition, rather than unscoped escape-code passthrough.

use std::io::{self, Write};
use std::sync::Arc;

use alacritty_terminal::term::graphics::{Image, Placement};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};
use serde::{Deserialize, Serialize};

use super::graphics_diacritics::DIACRITICS;

pub const MARKER: char = '\u{10eeee}';
const MAX_GRAPHICS: usize = 128;
const MAX_SCENE_BYTES: usize = 48 * 1024 * 1024;

#[derive(Clone)]
pub struct Graphic {
    pub key: String,
    pub image: Arc<Image>,
    pub crop: [u32; 4],
    pub size: [u32; 2],
}

impl PartialEq for Graphic {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.crop == other.crop && self.size == other.size
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct GraphicUpdate {
    pub key: String,
    pub image: Option<Arc<Image>>,
    pub crop: [u32; 4],
    pub size: [u32; 2],
}

pub fn updates(scene: &[Graphic], previous: &[Graphic]) -> Vec<GraphicUpdate> {
    scene
        .iter()
        .map(|graphic| GraphicUpdate {
            key: graphic.key.clone(),
            image: (!previous.iter().any(|old| old.key == graphic.key))
                .then(|| Arc::clone(&graphic.image)),
            crop: graphic.crop,
            size: graphic.size,
        })
        .collect()
}

pub fn apply_updates(
    previous: &[Graphic],
    updates: Vec<GraphicUpdate>,
) -> io::Result<Vec<Graphic>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid graphic frame");
    if updates.len() > MAX_GRAPHICS {
        return Err(invalid());
    }
    let mut bytes = 0usize;
    let mut result: Vec<Graphic> = Vec::new();
    for update in updates {
        if update.key.len() > 256
            || update.key.is_empty()
            || update.key.chars().any(char::is_control)
            || result.iter().any(|item| item.key == update.key)
        {
            return Err(invalid());
        }
        let image = if let Some(image) = update.image {
            // Payload validation is once per received image, not once per
            // text/cursor update referencing an already validated Arc.
            image.validate().map_err(|_| invalid())?;
            image
        } else {
            previous
                .iter()
                .find(|old| old.key == update.key)
                .map(|old| Arc::clone(&old.image))
                .ok_or_else(invalid)?
        };
        bytes = bytes.saturating_add(image.data.len());
        let [x, y, w, h] = update.crop;
        if bytes > MAX_SCENE_BYTES
            || image.width == 0
            || image.height == 0
            || u64::from(image.width) * u64::from(image.height) > 16 * 1024 * 1024
            || !matches!(image.format, 24 | 32 | 100)
            || w == 0
            || h == 0
            || x.checked_add(w).is_none_or(|end| end > image.width)
            || y.checked_add(h).is_none_or(|end| end > image.height)
            || update.crop != [0, 0, image.width, image.height]
            || update
                .size
                .iter()
                .any(|size| *size == 0 || *size as usize > DIACRITICS.len())
        {
            return Err(invalid());
        }
        result.push(Graphic {
            key: update.key,
            image,
            crop: update.crop,
            size: update.size,
        });
    }
    Ok(result)
}

pub fn add(scene: &mut Vec<Graphic>, graphic: Graphic) -> Option<u32> {
    if let Some(index) = scene.iter().position(|existing| existing == &graphic) {
        return Some(index as u32 + 1);
    }
    if scene.len() >= MAX_GRAPHICS
        || scene
            .iter()
            .map(|g| g.image.data.len())
            .sum::<usize>()
            .saturating_add(graphic.image.data.len())
            > MAX_SCENE_BYTES
    {
        return None;
    }
    scene.push(graphic);
    Some(scene.len() as u32)
}

pub fn index(symbol: &str, color: Color) -> Option<usize> {
    if !symbol.starts_with(MARKER) {
        return None;
    }
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    ((u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)) as usize).checked_sub(1)
}

pub fn slot_color(slot: u32) -> Color {
    Color::Rgb((slot >> 16) as u8, (slot >> 8) as u8, slot as u8)
}

/// Remap an application's explicit Unicode placeholder to a composed scene
/// slot. The actual text cell owns position, clipping and scrollback; never
/// paint a rectangle for virtual resources or reuse child IDs on the display.
pub fn virtual_cell(
    symbol: &str,
    foreground: Color,
    namespace: &str,
    placements: &[Placement],
    scene: &mut Vec<Graphic>,
) -> Option<(String, Color)> {
    let mut chars = symbol.chars();
    if chars.next()? != MARKER {
        return None;
    }
    let row_char = chars.next()?;
    let row = DIACRITICS.iter().position(|mark| *mark == row_char)?;
    let col_char = chars.next()?;
    let col = DIACRITICS.iter().position(|mark| *mark == col_char)?;
    let high = match chars.next() {
        None => 0,
        Some(mark) => u32::try_from(DIACRITICS.iter().position(|ch| *ch == mark)?).ok()?,
    };
    if high > 255 || chars.next().is_some() {
        return None;
    }
    let low = match foreground {
        Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
        Color::Indexed(index) => u32::from(index),
        _ => return None,
    };
    let image_id = low | high << 24;
    let p = placements
        .iter()
        .rev()
        .find(|p| p.virtual_placement && p.image_id == image_id && p.id == 0)?;
    if row >= p.rows as usize || col >= p.cols as usize {
        return None;
    }
    let slot = add(
        scene,
        Graphic {
            key: format!("{namespace}:{}:{}:{}", p.image.sequence, p.cols, p.rows),
            image: Arc::clone(&p.image),
            crop: [0, 0, p.image.width, p.image.height],
            size: [p.cols, p.rows],
        },
    )?;
    Some((
        format!("{MARKER}{}{}", DIACRITICS[row], DIACRITICS[col]),
        slot_color(slot),
    ))
}

pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    namespace: &str,
    placements: &[Placement],
    scene: &mut Vec<Graphic>,
) {
    for p in placements {
        if p.virtual_placement {
            continue;
        }
        // The parser rejects unsupported crops/oversize placements. Keep this
        // boundary defensive too: U=1 ignores crops, so tiling is not valid.
        if p.rows == 0
            || p.cols == 0
            || p.rows as usize > DIACRITICS.len()
            || p.cols as usize > DIACRITICS.len()
            || [p.x, p.y, p.width, p.height] != [0, 0, p.image.width, p.image.height]
        {
            continue;
        }
        let start_y = i64::from(area.y) + i64::from(p.row);
        let start_x = u64::from(area.x) + u64::from(p.col);
        if start_y >= i64::from(area.bottom())
            || start_y + i64::from(p.rows) <= i64::from(area.y)
            || start_x >= u64::from(area.right())
        {
            continue;
        }
        let graphic = Graphic {
            key: format!("{namespace}:{}:{}:{}", p.image.sequence, p.cols, p.rows),
            image: Arc::clone(&p.image),
            crop: [0, 0, p.image.width, p.image.height],
            size: [p.cols, p.rows],
        };
        let Some(slot) = add(scene, graphic) else {
            continue;
        };
        for row in 0..p.rows {
            let owner_y = i64::from(p.row) + i64::from(row);
            let dest_y = i64::from(area.y) + owner_y;
            if owner_y < i64::from(p.clip_top)
                || owner_y >= i64::from(p.clip_bottom)
                || dest_y < i64::from(area.y)
                || dest_y >= i64::from(area.bottom())
            {
                continue;
            }
            for col in 0..p.cols {
                let dest_x = start_x + u64::from(col);
                if dest_x >= u64::from(area.right()) {
                    break;
                }
                if let Some(cell) = buf.cell_mut((dest_x as u16, dest_y as u16)) {
                    cell.set_symbol(&format!(
                        "{MARKER}{}{}",
                        DIACRITICS[row as usize], DIACRITICS[col as usize]
                    ));
                    cell.set_fg(slot_color(slot));
                    cell.modifier = Modifier::empty();
                }
            }
        }
    }
}

/// Per-terminal-client resources. IDs are independent of child pane IDs and
/// every deletion is scoped to resources allocated by this display client.
#[derive(Default)]
pub struct ClientGraphics {
    pub scene: Vec<Graphic>,
    ids: Vec<u32>,
}

/// Kitty encodes placeholder identity in RGB, even in a colorless UI. Scope
/// Crossterm's override to the single display-thread paint, keeping ordinary
/// text colorless and restoring the previous policy on errors as well.
pub struct MetadataColors(bool);

impl MetadataColors {
    pub fn prepare(cells: &mut [(u16, u16, ratatui::buffer::Cell)]) -> Self {
        use ratatui::crossterm::style::Colored;
        let needed = Colored::ansi_color_disabled_memoized()
            && cells
                .iter()
                .any(|(_, _, cell)| cell.symbol().starts_with(MARKER));
        if needed {
            for (_, _, cell) in cells {
                cell.bg = Color::Reset;
                if !cell.symbol().starts_with(MARKER) {
                    cell.fg = Color::Reset;
                }
            }
            Colored::set_ansi_color_disabled(false);
        }
        Self(needed)
    }
}

impl Drop for MetadataColors {
    fn drop(&mut self) {
        if self.0 {
            ratatui::crossterm::style::Colored::set_ansi_color_disabled(true);
        }
    }
}

impl ClientGraphics {
    pub fn apply(
        &mut self,
        updates: Vec<GraphicUpdate>,
        output: &mut impl Write,
    ) -> io::Result<()> {
        let next = apply_updates(&self.scene, updates)?;
        let mut ids = Vec::new();
        for (slot, graphic) in next.iter().enumerate() {
            if self.scene.get(slot).is_some_and(|old| old == graphic) {
                ids.push(self.ids[slot]);
                continue;
            }
            // Text cells encode scene slots, not immutable image versions.
            // Reuse the terminal ID at that slot even when a frame replaces or
            // reorders images. Otherwise unchanged cells would still reference
            // a deleted image until the next full text repaint.
            let id = match self.ids.get(slot) {
                Some(id) => *id,
                None => loop {
                    let mut bytes = [0; 4];
                    getrandom::fill(&mut bytes)
                        .map_err(|error| io::Error::other(error.to_string()))?;
                    let id = u32::from_le_bytes(bytes).max(1);
                    if !self.ids.contains(&id) && !ids.contains(&id) {
                        break id;
                    }
                },
            };
            let image = &graphic.image;
            for (index, chunk) in image.data.as_bytes().chunks(4096).enumerate() {
                let more = u8::from((index + 1) * 4096 < image.data.len());
                if index == 0 {
                    write!(
                        output,
                        "\x1b_Ga=t,t=d,i={id},f={},s={},v={},q=2,m={more}{};",
                        image.format,
                        image.width,
                        image.height,
                        if image.compressed { ",o=z" } else { "" }
                    )?;
                } else {
                    write!(output, "\x1b_Gm={more},q=2;")?;
                }
                output.write_all(chunk)?;
                output.write_all(b"\x1b\\")?;
            }
            let [cols, rows] = graphic.size;
            write!(output, "\x1b_Ga=p,U=1,i={id},q=2,c={cols},r={rows};\x1b\\")?;
            ids.push(id);
        }
        for old in &self.ids {
            if !ids.contains(old) {
                write!(output, "\x1b_Ga=d,d=I,i={old},q=2;\x1b\\")?;
            }
        }
        self.ids = ids;
        self.scene = next;
        Ok(())
    }

    pub fn translate(&self, cells: &mut [(u16, u16, ratatui::buffer::Cell)]) {
        for (_, _, cell) in cells {
            if let Some(slot) = index(cell.symbol(), cell.fg) {
                if let Some(id) = self.ids.get(slot) {
                    let mut symbol: String = cell.symbol().chars().take(3).collect();
                    symbol.push(DIACRITICS[(id >> 24) as usize]);
                    cell.set_symbol(&symbol);
                    cell.set_fg(slot_color(*id & 0xffffff));
                } else {
                    cell.set_symbol(" ");
                }
            }
        }
    }
}

pub fn terminal_cell_pixels() -> (u16, u16) {
    crossterm::terminal::window_size()
        .ok()
        .and_then(|size| {
            let width = size.width.checked_div(size.columns)?;
            let height = size.height.checked_div(size.rows)?;
            ((1..=512).contains(&width) && (1..=512).contains(&height)).then_some((width, height))
        })
        .unwrap_or((8, 16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::graphics::Graphics;

    fn canvas() -> Graphics {
        let mut graphics = Graphics::default();
        let (reply, advance) =
            graphics.command(b"Ga=T,i=7,f=24,s=1,v=1,c=6,r=3,C=1;/wAA", 0, 0, (8, 16));
        assert!(reply.unwrap().contains("i=7;OK"));
        assert!(advance.is_none());
        graphics
    }

    #[test]
    fn kitty_queries_are_read_only_and_do_not_open_file_or_shm_paths() {
        let mut graphics = Graphics::default();
        for medium in ["f", "t", "s"] {
            let command = format!("Gi=4,a=q,t={medium},f=24,s=1,v=1;L2V0Yy9wYXNzd2Q=");
            let reply = graphics
                .command(command.as_bytes(), 0, 0, (8, 16))
                .0
                .unwrap();
            assert!(reply.contains("ENOTSUP"));
        }
        let reply = graphics
            .command(b"Gi=4,a=q,t=d,f=24,s=1,v=1;AAAA", 0, 0, (8, 16))
            .0
            .unwrap();
        assert!(reply.contains("i=4;OK"));
        assert!(graphics.visible().is_empty());
        assert!(graphics
            .command(b"Ga=p,i=4;", 0, 0, (8, 16))
            .0
            .unwrap()
            .contains("ENOENT"));
    }

    #[test]
    fn kitty_chunk_completion_tracks_final_cursor_and_rejects_excessive_dimensions() {
        let mut graphics = Graphics::default();
        assert!(graphics
            .command(b"Ga=T,i=7,f=24,s=2,v=1,c=2,r=1,C=1,m=1;/wAA", 0, 0, (8, 16))
            .0
            .is_none());
        assert!(graphics.visible().is_empty());
        assert!(graphics
            .command(b"Gm=0;AP8A", 2, 3, (8, 16))
            .0
            .unwrap()
            .contains("OK"));
        assert_eq!(graphics.visible()[0].row, 2);
        assert_eq!(graphics.visible()[0].col, 3);
        let reply = graphics
            .command(
                b"Ga=T,i=9,f=24,s=4294967295,v=4294967295;AAAA",
                0,
                0,
                (8, 16),
            )
            .0
            .unwrap();
        assert!(reply.contains("ENOSPC"));
        assert_eq!(graphics.visible().len(), 1);
    }

    #[test]
    fn kitty_composition_clips_to_pane_and_modal_cells_occlude_placeholders() {
        use ratatui::widgets::{Clear, Widget};
        let graphics = canvas();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 12));
        let mut scene = Vec::new();
        let area = Rect::new(7, 4, 4, 2);
        draw(&mut buffer, area, "owner-a", graphics.visible(), &mut scene);
        assert_eq!(scene.len(), 1);
        for y in 0..12 {
            for x in 0..20 {
                assert_eq!(
                    buffer[(x, y)].symbol().starts_with(MARKER),
                    area.contains((x, y).into())
                );
            }
        }
        Clear.render(Rect::new(8, 4, 2, 1), &mut buffer);
        assert!(!buffer[(8, 4)].symbol().starts_with(MARKER));
        assert!(buffer[(7, 4)].symbol().starts_with(MARKER));
        let mut other = Vec::new();
        draw(
            &mut buffer,
            Rect::new(0, 0, 4, 2),
            "owner-b",
            graphics.visible(),
            &mut other,
        );
        assert_ne!(
            scene[0].key, other[0].key,
            "the same child image ID on two owners must not collide"
        );
    }

    #[test]
    fn kitty_wire_cache_reuses_immutable_images_and_deletes_only_owned_ids() {
        let graphics = canvas();
        let mut scene = Vec::new();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        draw(
            &mut buffer,
            Rect::new(0, 0, 8, 4),
            "owner-a",
            graphics.visible(),
            &mut scene,
        );
        let first = updates(&scene, &[]);
        let retained = apply_updates(&[], first).unwrap();
        assert!(updates(&scene, &retained)[0].image.is_none());
        let mut client = ClientGraphics::default();
        let mut wire = Vec::new();
        client.apply(updates(&scene, &[]), &mut wire).unwrap();
        assert!(String::from_utf8(wire.clone()).unwrap().contains("a=p,U=1"));
        let mut cells = vec![(0, 0, buffer[(0, 0)].clone())];
        client.translate(&mut cells);
        assert_eq!(cells[0].2.symbol().chars().count(), 4);
        wire.clear();
        client.apply(updates(&scene, &scene), &mut wire).unwrap();
        assert!(
            wire.is_empty(),
            "unchanged frames must not retransmit image bytes"
        );
        client.apply(updates(&scene, &[]), &mut wire).unwrap();
        assert!(
            wire.is_empty(),
            "full text resync preserves cached image resources"
        );
        client.apply(Vec::new(), &mut wire).unwrap();
        let wire = String::from_utf8(wire).unwrap();
        assert!(wire.contains("a=d,d=I,i="));
        assert!(!wire.contains("d=A"));
    }

    #[test]
    fn kitty_image_updates_reuse_display_slots_without_repainting_placeholders() {
        let mut source = canvas();
        let mut client = ClientGraphics::default();
        let mut wire = Vec::new();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        let mut first = Vec::new();
        draw(
            &mut buffer,
            Rect::new(0, 0, 8, 4),
            "owner",
            source.visible(),
            &mut first,
        );
        client.apply(updates(&first, &[]), &mut wire).unwrap();
        let mut before = vec![(0, 0, buffer[(0, 0)].clone())];
        client.translate(&mut before);
        source.command(b"Ga=T,i=7,f=24,s=1,v=1,c=6,r=3,C=1;AP8A", 0, 0, (8, 16));
        let mut next = Vec::new();
        draw(
            &mut buffer,
            Rect::new(0, 0, 8, 4),
            "owner",
            source.visible(),
            &mut next,
        );
        wire.clear();
        client.apply(updates(&next, &first), &mut wire).unwrap();
        let mut after = vec![(0, 0, buffer[(0, 0)].clone())];
        client.translate(&mut after);
        assert_eq!(
            before, after,
            "unchanged placeholder cells retain their display image ID"
        );
        let text = String::from_utf8(wire).unwrap();
        assert!(text.contains("AP8A"));
        assert!(!text.contains("a=d"), "updated slots must not be deleted");
    }

    #[test]
    fn kitty_images_clear_and_restore_with_alternate_screen_and_scroll() {
        let mut graphics = canvas();
        graphics.swap_alt(true);
        assert!(graphics.visible().is_empty());
        graphics.swap_alt(false);
        assert_eq!(graphics.visible().len(), 1);
        graphics.scroll(0, 24, -1);
        assert_eq!(graphics.visible()[0].row, -1);
        graphics.clear();
        assert!(graphics.visible().is_empty());
    }

    #[test]
    fn kitty_scrolled_clipping_keeps_original_source_rows_after_reverse_scroll() {
        assert_eq!(
            DIACRITICS.len(),
            alacritty_terminal::term::graphics::MAX_PLACEMENT_CELLS as usize
        );
        let mut graphics = canvas();
        graphics.scroll(0, 24, -1);
        graphics.scroll(0, 24, 1);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 12));
        let mut scene = Vec::new();
        draw(
            &mut buffer,
            Rect::new(7, 4, 6, 3),
            "owner",
            graphics.visible(),
            &mut scene,
        );
        assert!(!buffer[(7, 4)].symbol().starts_with(MARKER));
        assert_eq!(
            buffer[(7, 5)].symbol(),
            format!("{MARKER}{}{}", DIACRITICS[1], DIACRITICS[0])
        );
        assert_eq!(scene[0].crop, [0, 0, 1, 1]);
        assert_eq!(scene[0].size, [6, 3]);
    }

    #[test]
    fn kitty_peer_rejects_forged_png_dimensions_and_source_crops() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&10000u32.to_be_bytes());
        bytes.extend_from_slice(&10000u32.to_be_bytes());
        let update = GraphicUpdate {
            key: "peer:1".into(),
            image: Some(Arc::new(Image {
                sequence: 1,
                width: 1,
                height: 1,
                format: 100,
                compressed: false,
                data: crate::base64_encode(&bytes),
            })),
            crop: [0, 0, 1, 1],
            size: [1, 1],
        };
        assert!(apply_updates(&[], vec![update]).is_err());
        let image = Arc::new(Image {
            sequence: 1,
            width: 2,
            height: 1,
            format: 24,
            compressed: false,
            data: "/wAAAP8A".into(),
        });
        let cropped = GraphicUpdate {
            key: "peer:2".into(),
            image: Some(image),
            crop: [1, 0, 1, 1],
            size: [1, 1],
        };
        assert!(apply_updates(&[], vec![cropped]).is_err());
    }

    #[test]
    fn kitty_virtual_cells_own_position_crop_identity_and_reuse_after_erase() {
        let mut graphics = Graphics::default();
        let (reply, advance) = graphics.command(
            b"Ga=T,U=1,i=66055,f=24,s=1,v=1,c=16,r=7;/wAA",
            10,
            20,
            (8, 16),
        );
        assert!(reply.unwrap().contains(";OK"));
        assert!(advance.is_none());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        let mut scene = Vec::new();
        draw(
            &mut buffer,
            Rect::new(0, 0, 80, 24),
            "owner",
            graphics.visible(),
            &mut scene,
        );
        assert!(
            scene.is_empty(),
            "virtual resources must not paint cursor rectangles"
        );
        graphics.erase_screen();
        graphics.scroll(0, 24, -5);
        let symbol = format!("{MARKER}{}{}", DIACRITICS[4], DIACRITICS[8]);
        let (mapped, foreground) = virtual_cell(
            &symbol,
            Color::Rgb(1, 2, 7),
            "owner",
            graphics.visible(),
            &mut scene,
        )
        .unwrap();
        assert_eq!(
            mapped, symbol,
            "sliced windows preserve source cell coordinates"
        );
        assert_eq!(foreground, slot_color(1));
        assert_eq!(
            scene[0].size,
            [16, 7],
            "virtual target box is not ordinary stretch validation"
        );
        let previous = scene.clone();
        assert_eq!(
            graphics.visible()[0].row,
            10,
            "resource has no screen scroll position"
        );
        assert!(virtual_cell(
            &symbol,
            Color::Rgb(1, 2, 7),
            "other-owner",
            graphics.visible(),
            &mut scene
        )
        .is_some());
        assert_ne!(scene[0].key, scene[1].key);
        assert!(updates(&scene, &previous)[0].image.is_none());
        graphics.command(b"Ga=d,d=I,i=66055;", 0, 0, (8, 16));
        assert!(virtual_cell(
            &symbol,
            Color::Rgb(1, 2, 7),
            "owner",
            graphics.visible(),
            &mut scene
        )
        .is_none());
        for bad in [
            format!("{MARKER}"),
            format!("{MARKER}{}{}", DIACRITICS[7], DIACRITICS[0]),
        ] {
            assert!(virtual_cell(
                &bad,
                Color::Rgb(1, 2, 7),
                "owner",
                graphics.visible(),
                &mut scene
            )
            .is_none());
        }
    }

    #[test]
    fn kitty_virtual_prototypes_survive_delete_all_without_removing_anonymous_placements() {
        let mut graphics = canvas();
        let reply = graphics
            .command(b"Ga=p,U=1,i=7,c=6,r=3;", 0, 0, (8, 16))
            .0
            .unwrap();
        assert!(reply.contains(";OK"));
        assert_eq!(
            graphics.visible().len(),
            2,
            "ordinary anonymous placement survives virtual setup"
        );
        graphics.command(b"Ga=p,U=1,i=7,c=6,r=3;", 0, 0, (8, 16));
        assert_eq!(
            graphics.visible().len(),
            2,
            "virtual prototype is replaced, not accumulated"
        );
        graphics.command(b"Ga=d,d=A;", 0, 0, (8, 16));
        assert_eq!(graphics.visible().len(), 1);
        assert!(graphics.visible()[0].virtual_placement);
        let symbol = format!("{MARKER}{}{}", DIACRITICS[0], DIACRITICS[0]);
        assert!(virtual_cell(
            &symbol,
            Color::Rgb(0, 0, 7),
            "owner",
            graphics.visible(),
            &mut Vec::new()
        )
        .is_some());
        graphics.command(b"Ga=d,d=I,i=7;", 0, 0, (8, 16));
        assert!(graphics.visible().is_empty());
    }

    #[test]
    fn kitty_named_deletion_preserves_sibling_and_primary_screen_placements() {
        let mut graphics = canvas();
        for placement in [1, 2] {
            let command = format!("Ga=p,i=7,p={placement},C=1;");
            assert!(graphics
                .command(command.as_bytes(), 0, 0, (8, 16))
                .0
                .unwrap()
                .contains("OK"));
        }
        graphics.command(b"Ga=d,d=I,i=7,p=1;", 0, 0, (8, 16));
        assert_eq!(
            graphics.visible().iter().map(|p| p.id).collect::<Vec<_>>(),
            [0, 2]
        );
        graphics.swap_alt(true);
        assert!(graphics
            .command(b"Ga=p,i=7,p=3,C=1;", 0, 0, (8, 16))
            .0
            .unwrap()
            .contains("OK"));
        graphics.command(b"Ga=d,d=A;", 0, 0, (8, 16));
        assert!(graphics.visible().is_empty());
        graphics.swap_alt(false);
        assert_eq!(graphics.visible().len(), 2);
        assert!(graphics
            .command(b"Ga=p,i=7,p=4,C=1;", 0, 0, (8, 16))
            .0
            .unwrap()
            .contains("OK"));
    }

    #[test]
    fn kitty_invalid_replacement_preserves_previous_image_and_placements() {
        let mut graphics = canvas();
        let original = Arc::clone(&graphics.visible()[0].image);
        for option in ["x=4", "C=2", "z=-1"] {
            let command = format!("Ga=T,i=7,f=24,s=1,v=1,{option};AP8A");
            let reply = graphics
                .command(command.as_bytes(), 0, 0, (8, 16))
                .0
                .unwrap();
            assert!(!reply.contains(";OK"), "{reply}");
            assert!(Arc::ptr_eq(&graphics.visible()[0].image, &original));
        }
        assert!(graphics
            .command(b"Ga=T,i=7,f=24,s=1,v=1,C=1;AP8A", 0, 0, (8, 16))
            .0
            .unwrap()
            .contains(";OK"));
        assert!(!Arc::ptr_eq(&graphics.visible()[0].image, &original));
        assert_eq!(graphics.visible()[0].image.data, "AP8A");
    }

    #[test]
    fn kitty_anonymous_placements_accumulate_and_delete_aborts_chunked_upload() {
        let mut graphics = canvas();
        graphics.command(b"Ga=p,i=7,C=1;", 3, 4, (8, 16));
        assert_eq!(graphics.visible().len(), 2);
        assert_eq!(graphics.visible()[1].col, 4);
        graphics.command(b"Ga=T,i=8,f=24,s=2,v=1,m=1;/wAA", 0, 0, (8, 16));
        graphics.command(b"Ga=d,d=A;", 0, 0, (8, 16));
        graphics.command(b"Gm=0;AP8A", 0, 0, (8, 16));
        assert!(graphics.visible().is_empty());
        assert!(graphics
            .command(b"Ga=p,i=8;", 0, 0, (8, 16))
            .0
            .unwrap()
            .contains("ENOENT"));
    }
}
