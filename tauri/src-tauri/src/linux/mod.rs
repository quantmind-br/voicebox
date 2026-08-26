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
/// True when this process is talking to a Wayland compositor.
pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland"))
}

fn has_nvidia_driver() -> bool {
    std::path::Path::new("/proc/driver/nvidia/version").exists()
}

/// Environment fixes that must land before GTK or WebKitGTK initialise.
///
/// Call from the top of `main`, ahead of any Tauri setup — WebKitGTK reads
/// these once, when its process-wide renderer is chosen, and ignores later
/// changes.
///
/// The one fix here is WebKitGTK's DMABUF renderer on the NVIDIA driver,
/// whose failure mode is a window that maps but paints nothing. A blank
/// window is indistinguishable from a hung app, so the trade is a
/// straightforward one: disabling the renderer costs some compositing
/// performance, while leaving it on can cost the entire UI.
///
/// Both directions stay overridable. An explicit `WEBKIT_DISABLE_DMABUF_RENDERER`
/// in the environment always wins, and `VOICEBOX_WEBKIT_DMABUF=1` opts back
/// into the fast path on a driver/compositor pairing where it renders fine.
pub fn apply_webkit_workarounds() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some() {
        return;
    }
    if std::env::var("VOICEBOX_WEBKIT_DMABUF").is_ok_and(|v| v == "1") {
        return;
    }
    if !is_wayland() || !has_nvidia_driver() {
        return;
    }
    eprintln!(
        "[voicebox] NVIDIA driver on Wayland detected — disabling WebKitGTK's DMABUF \
         renderer to avoid a blank window. Set VOICEBOX_WEBKIT_DMABUF=1 to keep it on."
    );
    // SAFETY: called from the top of main, before any thread is spawned and
    // before GTK/WebKit read the environment. set_var is only unsound when it
    // races another thread's getenv, which cannot happen here.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

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
