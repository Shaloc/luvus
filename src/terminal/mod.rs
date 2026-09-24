//! Terminal layer: the PTY actor (`pty`) and the pure-Rust VT engine (`vt`).
//! See docs/05-pty-and-terminal.md.

pub mod appearance;
pub mod backend;
pub mod clipboard;
pub(crate) mod graphics;
mod graphics_diacritics;
#[cfg(any(windows, test))]
pub mod host_input;
pub mod host_key;
pub mod keyboard;
pub mod pty;
pub mod theme_probe;
pub mod upload;
pub mod vt;

/// Kitty can intentionally advertise a generic TERM (for remote terminfo
/// compatibility). Clipboard and graphics must recognize the same display.
pub(crate) fn is_kitty_display() -> bool {
    std::env::var("TERM").is_ok_and(|term| term.contains("kitty"))
        || std::env::var_os("KITTY_WINDOW_ID").is_some_and(|id| !id.is_empty())
}
