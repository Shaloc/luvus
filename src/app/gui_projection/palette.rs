use ratatui::style::Color;
use serde_json::{json, Value};

use crate::{ipc::protocol::pack, ui::theme::Theme};

/// Desktop-owned ANSI slots. TUI clients resolve these through their terminal
/// emulator instead. Never apply this palette to cells already carrying RGB.
pub(super) fn ansi(name: &str, theme: &Theme) -> Value {
    // The semantic theme has no blue/magenta slots. Supply complementary ANSI
    // colors explicitly, rather than turning ANSI blue into the focus accent.
    let (blue, magenta) = match crate::ui::theme::canonical(name) {
        "catppuccin-latte" => (0x1e66f5, 0x8839ef),
        "catppuccin-frappe" => (0x8caaee, 0xca9ee6),
        "catppuccin-macchiato" | "quattro-rally" => (0x8aadf4, 0xc6a0f6),
        "catppuccin-mocha" => (0x89b4fa, 0xcba6f7),
        "tokyo-night" => (0x7aa2f7, 0xbb9af7),
        "one-dark" => (0x61afef, 0xc678dd),
        "one-light" => (0x4078f2, 0xa626a4),
        "nord" => (0x81a1c1, 0xb48ead),
        "dracula" => (0xbd93f9, 0xff79c6),
        "gruvbox" => (0x83a598, 0xd3869b),
        "gruvbox-light" => (0x076678, 0x8f3f71),
        "rose-pine" => (0x31748f, 0xc4a7e7),
        "rose-pine-moon" => (0x3e8fb0, 0xc4a7e7),
        "rose-pine-dawn" => (0x286983, 0x907aa9),
        "papercolor" | "paper" | "sky" => (0x005faf, 0x8700af),
        "mono" => {
            return json!({"red":pack(theme.coral),"green":pack(theme.green),
            "yellow":pack(theme.amber),"blue":pack(theme.text),"magenta":pack(theme.text),"cyan":pack(theme.mint)})
        }
        _ => {
            return json!({"red":pack(theme.coral),"green":pack(theme.green),
            "yellow":pack(theme.amber),"cyan":pack(theme.mint)})
        }
    };
    let rgb = |value: u32| {
        pack(Color::Rgb(
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ))
    };
    json!({"red":pack(theme.coral),"green":pack(theme.green),"yellow":pack(theme.amber),
        "blue":rgb(blue),"magenta":rgb(magenta),"cyan":pack(theme.mint)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_ansi_slots_follow_selected_theme_without_reusing_its_accent() {
        let mocha = ansi("mocha", &Theme::mocha());
        assert_eq!(mocha["blue"], 0x0289b4fau32);
        assert_eq!(mocha["magenta"], pack(Theme::mocha().accent));
        let nord = ansi("nord", &Theme::nord());
        assert_ne!(nord["blue"], mocha["blue"]);
        assert_eq!(nord["green"], pack(Theme::nord().green));
        let rally = ansi("quattro-rally", &Theme::quattro_rally());
        assert_ne!(rally["blue"], pack(Theme::quattro_rally().accent));
    }
}
