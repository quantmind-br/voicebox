//! Platform permission gate for the auto-paste pipeline.
//!
//! On macOS, posting synthetic keyboard events and reading focused-UI state
//! via the AX API both require the host process to be listed under System
//! Settings → Privacy & Security → Accessibility. Without that trust,
//! `CGEventPost` silently drops events and `AXUIElementCopyAttributeValue`
//! returns an error. We surface a boolean check up front so the paste
//! pipeline can short-circuit with a clear "grant permission" message
//! instead of running through the full save → write → post → restore dance
//! with nothing to show for it.
//!
//! Windows has no equivalent user-facing permission — `SendInput` and
//! UIAutomation work for any non-elevated target out of the box. (UAC /
//! UIPI still blocks sending input *into* an elevated target window from a
//! non-elevated process, but that's per-target, not a global switch, and
//! there's no Settings pane to send users to.) So the Windows branch just
//! returns `true`.

#[cfg(target_os = "macos")]
mod ffi {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        /// Returns true when the current process is listed in Accessibility.
        /// No prompt side-effect.
        pub fn AXIsProcessTrusted() -> bool;
    }
}

#[cfg(target_os = "macos")]
pub fn is_trusted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() }
}

#[cfg(target_os = "windows")]
pub fn is_trusted() -> bool {
    true
}

#[cfg(target_os = "linux")]
pub fn is_trusted() -> bool {
    // No TCC-style switch exists here either, but unlike Windows the
    // capability genuinely can be absent: auto-paste needs both clipboard
    // access and a way to synthesise a keystroke, and a stock install may
    // have neither. Report what is actually true so the paste pipeline can
    // fail up front with an actionable message instead of getting all the way
    // to the keystroke and dropping it.
    crate::linux::auto_paste_capability().is_ok()
}

/// Why [`is_trusted`] is false, phrased for the user.
///
/// macOS and Windows have a single well-known answer ("open this Settings
/// pane" / "nothing to grant"), so only Linux needs to explain itself — the
/// fix there depends on which link of the paste chain is missing.
#[cfg(target_os = "linux")]
pub fn permission_hint() -> String {
    crate::linux::auto_paste_capability()
        .err()
        .unwrap_or_else(|| "Auto-paste is available.".to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn permission_hint() -> String {
    "Accessibility permission required for auto-paste. Open System Settings → \
     Privacy & Security → Accessibility and enable Voicebox."
        .to_string()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn is_trusted() -> bool {
    false
}
