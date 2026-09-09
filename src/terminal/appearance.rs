//! Effective pane appearance exposed to child terminal applications.
//!
//! Static themes provide an explicit dark/light preference. The virtual
//! Terminal theme instead follows the last host-terminal probe. Each VT engine
//! owns a copy so OSC 10/11 and DEC mode 2031 stay correct without process-global
//! mutable state.

use ratatui::style::Color;

use crate::terminal::theme_probe::TerminalColors;
use crate::theme::format::Appearance as ThemeAppearance;

/// Bundled fallback pane background (Quattro Rally mantle) used until a
/// Terminal-theme host probe is available.
pub const DEFAULT_BG: [u8; 3] = [0x1e, 0x20, 0x30];
pub const DEFAULT_FG: [u8; 3] = [0xca, 0xd3, 0xf5];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScheme {
    Dark,
    Light,
}

impl ColorScheme {
    /// Infer the virtual Terminal theme's scheme from its probed foreground and
    /// background. Static themes use their declared appearance instead.
    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        if luminance(colors.bg) < luminance(colors.fg) {
            Self::Dark
        } else {
            Self::Light
        }
    }

    pub fn is_dark(self) -> bool {
        self == Self::Dark
    }

    pub fn dsr(self) -> &'static [u8] {
        match self {
            Self::Dark => b"\x1b[?997;1n",
            Self::Light => b"\x1b[?997;2n",
        }
    }
}

/// The default colors and declared color-scheme preference visible to one child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneAppearance {
    // None means a host-defined ANSI entry whose RGB has not been negotiated.
    // An unanswered query is preferable to reporting an unrelated default.
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub scheme: ColorScheme,
}

impl PaneAppearance {
    pub fn resolve(
        foreground_color: Color,
        background_color: Color,
        declared: ThemeAppearance,
        probed: Option<Self>,
    ) -> Self {
        let foreground = query_color(
            foreground_color,
            probed.and_then(|p| p.foreground).unwrap_or(DEFAULT_FG),
        );
        let background = query_color(
            background_color,
            probed.and_then(|p| p.background).unwrap_or(DEFAULT_BG),
        );
        let scheme = match declared {
            ThemeAppearance::Dark => ColorScheme::Dark,
            ThemeAppearance::Light => ColorScheme::Light,
            ThemeAppearance::Terminal => probed
                .map(|appearance| appearance.scheme)
                .unwrap_or(ColorScheme::Dark),
        };
        Self {
            foreground,
            background,
            scheme,
        }
    }

    pub fn from_terminal_colors(colors: &TerminalColors) -> Self {
        Self {
            foreground: Some(colors.fg),
            background: Some(colors.bg),
            scheme: ColorScheme::from_terminal_colors(colors),
        }
    }
}

impl Default for PaneAppearance {
    fn default() -> Self {
        Self {
            foreground: Some(DEFAULT_FG),
            background: Some(DEFAULT_BG),
            scheme: ColorScheme::Dark,
        }
    }
}

fn luminance(rgb: [u8; 3]) -> f32 {
    0.2126 * (rgb[0] as f32 / 255.0)
        + 0.7152 * (rgb[1] as f32 / 255.0)
        + 0.0722 * (rgb[2] as f32 / 255.0)
}

fn query_color(color: Color, default: [u8; 3]) -> Option<[u8; 3]> {
    match color {
        Color::Reset => Some(default),
        Color::Rgb(r, g, b) => Some([r, g, b]),
        Color::Indexed(index @ 232..=255) => Some([8 + (index - 232) * 10; 3]),
        Color::Indexed(index @ 16..=231) => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let cube = (index - 16) as usize;
            Some([LEVELS[cube / 36], LEVELS[cube / 6 % 6], LEVELS[cube % 6]])
        }
        // The first 16 entries are host-defined. Do not invent their RGB or
        // confuse them with the default foreground/background from OSC 10/11.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_theme_queries_use_the_fixed_xterm_color() {
        let appearance = PaneAppearance::resolve(
            Color::Indexed(196),
            Color::Indexed(244),
            ThemeAppearance::Dark,
            None,
        );
        assert_eq!(appearance.foreground, Some([255, 0, 0]));
        assert_eq!(appearance.background, Some([128, 128, 128]));
        assert_eq!(query_color(Color::Indexed(16), DEFAULT_FG), Some([0; 3]));
        assert_eq!(query_color(Color::Indexed(231), DEFAULT_FG), Some([255; 3]));
        assert_eq!(query_color(Color::Indexed(232), DEFAULT_FG), Some([8; 3]));
        assert_eq!(query_color(Color::Indexed(255), DEFAULT_FG), Some([238; 3]));
        for index in 0..16 {
            assert_eq!(query_color(Color::Indexed(index), DEFAULT_FG), None);
        }
    }

    #[test]
    fn classifies_probed_terminal_colors_from_their_contrast() {
        let mut colors = TerminalColors {
            fg: [0xca, 0xd3, 0xf5],
            bg: [0x1e, 0x20, 0x30],
            palette: [[0; 3]; 16],
        };
        assert_eq!(
            ColorScheme::from_terminal_colors(&colors),
            ColorScheme::Dark
        );
        colors.fg = [0x4c, 0x4f, 0x69];
        colors.bg = [0xef, 0xf1, 0xf5];
        assert_eq!(
            ColorScheme::from_terminal_colors(&colors),
            ColorScheme::Light
        );
        assert_eq!(ColorScheme::Dark.dsr(), b"\x1b[?997;1n");
        assert_eq!(ColorScheme::Light.dsr(), b"\x1b[?997;2n");
    }

    #[test]
    fn static_theme_declaration_outranks_color_guessing() {
        assert_eq!(
            PaneAppearance::resolve(
                Color::Rgb(0x11, 0x11, 0x11),
                Color::Rgb(0xee, 0xee, 0xee),
                ThemeAppearance::Dark,
                None,
            )
            .scheme,
            ColorScheme::Dark
        );
    }

    #[test]
    fn terminal_reset_uses_probe_then_fallback() {
        let probed = PaneAppearance {
            foreground: Some([0x28, 0x28, 0x28]),
            background: Some([0xf2, 0xe5, 0xbc]),
            scheme: ColorScheme::Light,
        };
        assert_eq!(
            PaneAppearance::resolve(
                Color::Reset,
                Color::Reset,
                ThemeAppearance::Terminal,
                Some(probed)
            ),
            probed
        );
        assert_eq!(
            PaneAppearance::resolve(Color::Reset, Color::Reset, ThemeAppearance::Terminal, None),
            PaneAppearance::default()
        );
    }
}
