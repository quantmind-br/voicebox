//! Linux/Wayland platform backends.
//!
//! The auto-paste pipeline needs four capabilities that macOS and Windows
//! each expose through a single system API: read the focused window, raise a
//! window, own the clipboard, and synthesise a keystroke. Wayland grants a
//! client none of them, on purpose — they are compositor privileges. So each
//! one is rebuilt here from the mechanism that *is* available:
//!
//! | capability        | mechanism                                  |
//! |-------------------|--------------------------------------------|
//! | focused window    | [`hyprland`] IPC (`activewindow`)          |
//! | raise a window    | [`hyprland`] IPC (`dispatch focuswindow`)  |
//! | clipboard         | [`clipboard`] via `wlr-`/`ext-data-control`|
//! | synthetic Ctrl+V  | [`paste`] via `wtype`, else `/dev/uinput`  |
//!
//! Only the first two are compositor-specific. On a non-Hyprland session the
//! clipboard and keystroke halves still work, so dictation degrades to
//! "paste into whatever has focus" rather than failing outright — which is
//! the correct behaviour anyway, because the pill is configured not to take
//! focus in the first place.

pub mod clipboard;
pub mod hyprland;
pub mod paste;

/// Whether the auto-paste pipeline can run at all, and why not if it can't.
///
/// Auto-paste is a chain: stage the transcript on the clipboard, then
/// synthesise the accelerator that makes the focused app read it. Both links
/// are separately optional on a stock Linux install — a compositor may not
/// implement a data-control protocol, and a machine may have neither `wtype`
/// nor a writable `/dev/uinput` — so both are checked, and the message names
/// the one that is actually missing rather than a generic failure.
pub fn auto_paste_capability() -> Result<(), String> {
    match (clipboard::is_available(), paste::is_available()) {
        (true, true) => Ok(()),
        (false, true) => Err("Auto-paste needs clipboard access, which requires the \
             compositor to support the wlr-data-control or ext-data-control protocol. \
             Hyprland, Sway and other wlroots compositors do; GNOME does not."
            .to_string()),
        (true, false) => Err(paste::unavailable_reason()),
        (false, false) => Err(format!(
            "Auto-paste is unavailable: this compositor exposes no clipboard \
             data-control protocol, and no keystroke injection method is usable. {}",
            paste::unavailable_reason()
        )),
    }
}
