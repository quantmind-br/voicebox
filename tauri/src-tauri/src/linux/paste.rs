//! Synthetic paste accelerator for Wayland.
//!
//! Wayland has no XTEST, so there is no protocol a normal client can use to
//! say "pretend the user pressed Ctrl+V". Two mechanisms exist below that
//! ceiling, and this module uses both, in order:
//!
//! 1. **`wtype`** — a tiny helper that binds `zwp_virtual_keyboard_v1` and
//!    uploads *its own* xkb keymap before sending the keystroke. That upload
//!    is why it is tried first: it makes the injected key layout-correct by
//!    construction. A raw keycode does not have that property. Keycodes are
//!    physical positions, so `KEY_V` on a Dvorak layout arrives as `.`, and
//!    `Ctrl+.` is not a paste. This is the same trap `keyboard_layout.rs`
//!    documents on the macOS side, where Cmd+V is matched by translated
//!    character rather than by keycode.
//!
//! 2. **`/dev/uinput`** — a kernel-level virtual keyboard, used when `wtype`
//!    is not installed. It needs no compositor protocol at all, which makes
//!    it the more universal of the two, but it emits raw keycodes and so
//!    carries exactly the layout caveat above. Fine on the QWERTY-family
//!    layouts that put `v` on `KEY_V`; wrong on Dvorak and friends, hence
//!    second.
//!
//! The uinput device is created once and kept alive for the life of the
//! process. Creating one per paste would be cleaner to reason about but does
//! not work: the compositor learns about a new input device asynchronously
//! via libinput, and a device that is created, used and destroyed inside a
//! few milliseconds usually has its events dropped because nothing was
//! listening yet.

use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, InputEvent, KeyCode};

/// How long to wait after creating the virtual keyboard before trusting it
/// to deliver. Covers udev enumeration plus the compositor's libinput
/// device-added round trip. Paid once per process, on the first paste.
const DEVICE_SETTLE: Duration = Duration::from_millis(250);

/// Gap between individual key transitions. Some toolkits debounce or drop
/// modifier+key pairs that arrive in the same millisecond.
const KEY_INTERVAL: Duration = Duration::from_millis(12);

/// Which accelerator the focused application expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accelerator {
    /// Ctrl+V — every GTK/Qt/Electron/web surface.
    Standard,
    /// Ctrl+Shift+V — terminal emulators, where plain Ctrl+V is the
    /// readline "quoted insert" and pasting into it would insert a control
    /// character rather than the transcript.
    Terminal,
}

/// Terminal emulators, by Wayland app-id / X11 class, that take
/// Ctrl+Shift+V rather than Ctrl+V.
///
/// Matched as a lowercase substring so reverse-DNS ids
/// (`com.mitchellh.ghostty`) and bare classes (`ghostty`) both land. Being
/// wrong in the "not a terminal" direction is the safer failure: Ctrl+V in a
/// terminal inserts a stray control character, while Ctrl+Shift+V in a text
/// editor is almost always either paste-as-plain-text or a no-op.
const TERMINAL_CLASS_MARKERS: &[&str] = &[
    "alacritty",
    "foot",
    "ghostty",
    "gnome-terminal",
    "konsole",
    "kitty",
    "org.wezfurlong.wezterm",
    "rio",
    "st-256color",
    "terminator",
    "termite",
    "tilix",
    "urxvt",
    "wezterm",
    "xfce4-terminal",
    "xterm",
];

/// Pick the accelerator for a focused-window class.
///
/// `None` — an unknown or unreported class — takes the standard binding,
/// which is right for everything that is not a terminal.
pub fn accelerator_for_class(class: Option<&str>) -> Accelerator {
    let Some(class) = class else {
        return Accelerator::Standard;
    };
    let class = class.to_ascii_lowercase();
    if TERMINAL_CLASS_MARKERS
        .iter()
        .any(|marker| class.contains(marker))
    {
        Accelerator::Terminal
    } else {
        Accelerator::Standard
    }
}

/// True when at least one injection mechanism is usable right now.
///
/// Drives the permission gate in the UI, so it must not have side effects —
/// no device is created and no key is sent.
pub fn is_available() -> bool {
    wtype_path().is_some() || uinput_is_writable()
}

/// Human-readable reason why [`is_available`] is false, for the settings UI.
pub fn unavailable_reason() -> String {
    "Auto-paste needs a way to synthesise Ctrl+V. Install `wtype` \
     (pacman -S wtype), or grant access to /dev/uinput by adding your user to \
     the `input` group and installing the udev rule shipped in \
     packaging/99-voicebox-uinput.rules."
        .to_string()
}

/// Send the paste accelerator to whatever currently holds keyboard focus.
pub fn send_paste(accelerator: Accelerator) -> Result<(), String> {
    let mut errors = Vec::new();

    if wtype_path().is_some() {
        match send_via_wtype(accelerator) {
            Ok(()) => return Ok(()),
            Err(e) => errors.push(format!("wtype: {e}")),
        }
    }

    match send_via_uinput(accelerator) {
        Ok(()) => return Ok(()),
        Err(e) => errors.push(format!("uinput: {e}")),
    }

    Err(if errors.is_empty() {
        unavailable_reason()
    } else {
        format!("{}. {}", errors.join("; "), unavailable_reason())
    })
}

// ========================================================================
// wtype
// ========================================================================

fn wtype_path() -> Option<std::path::PathBuf> {
    // Cheap enough to re-resolve; the user may install wtype without
    // restarting the app, and caching a negative answer would hide that.
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("wtype"))
            .find(|candidate| candidate.is_file())
    })
}

fn send_via_wtype(accelerator: Accelerator) -> Result<(), String> {
    // -M presses a modifier, -m releases it, -k taps a keysym. Releasing
    // explicitly matters: wtype exits immediately afterwards, and a modifier
    // left down would stick for the compositor.
    let mut args: Vec<&str> = vec!["-M", "ctrl"];
    if accelerator == Accelerator::Terminal {
        args.extend_from_slice(&["-M", "shift"]);
    }
    args.extend_from_slice(&["-k", "v"]);
    if accelerator == Accelerator::Terminal {
        args.extend_from_slice(&["-m", "shift"]);
    }
    args.extend_from_slice(&["-m", "ctrl"]);

    let output = Command::new("wtype")
        .args(&args)
        .output()
        .map_err(|e| format!("could not run wtype: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "wtype exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

// ========================================================================
// uinput
// ========================================================================

fn uinput_is_writable() -> bool {
    // An open-for-write probe is the only honest test: the mode bits alone
    // miss both ACLs (systemd-logind commonly grants the seat owner access
    // that way) and the case where the module is not loaded at all.
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

/// The process-wide virtual keyboard, created on first use.
///
/// `None` inside the mutex means "not built yet"; a failed build is not
/// cached, so a user who fixes permissions mid-session gets a working paste
/// without restarting the app.
static DEVICE: Mutex<Option<VirtualDevice>> = Mutex::new(None);

fn send_via_uinput(accelerator: Accelerator) -> Result<(), String> {
    let mut guard = DEVICE
        .lock()
        .map_err(|_| "virtual keyboard mutex was poisoned".to_string())?;

    if guard.is_none() {
        *guard = Some(build_device()?);
        // First use only — see DEVICE_SETTLE.
        std::thread::sleep(DEVICE_SETTLE);
    }
    let device = guard.as_mut().expect("device was just built");

    let mut modifiers = vec![KeyCode::KEY_LEFTCTRL];
    if accelerator == Accelerator::Terminal {
        modifiers.push(KeyCode::KEY_LEFTSHIFT);
    }

    // Any early return between the presses below would strand a modifier in
    // the held state for every application on the system, so releases run
    // unconditionally and only the first error is reported.
    let mut result = Ok(());
    for key in &modifiers {
        result = result.and(tap(device, *key, 1));
    }
    result = result.and(tap(device, KeyCode::KEY_V, 1));
    result = result.and(tap(device, KeyCode::KEY_V, 0));
    for key in modifiers.iter().rev() {
        let release = tap(device, *key, 0);
        result = result.and(release);
    }
    result
}

fn tap(device: &mut VirtualDevice, key: KeyCode, value: i32) -> Result<(), String> {
    let event = *evdev::KeyEvent::new(key, value);
    device
        .emit(&[InputEvent::from(event)])
        .map_err(|e| format!("could not emit {key:?}={value}: {e}"))?;
    std::thread::sleep(KEY_INTERVAL);
    Ok(())
}

fn build_device() -> Result<VirtualDevice, String> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_LEFTCTRL);
    keys.insert(KeyCode::KEY_LEFTSHIFT);
    keys.insert(KeyCode::KEY_V);

    VirtualDevice::builder()
        .map_err(|e| format!("could not open /dev/uinput: {e}"))?
        // Shows up in `libinput list-devices` and in the compositor's device
        // list, so name it after the app rather than something generic.
        .name("Voicebox Virtual Keyboard")
        .with_keys(&keys)
        .map_err(|e| format!("could not declare keys on the virtual keyboard: {e}"))?
        .build()
        .map_err(|e| format!("could not create the virtual keyboard: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_get_the_shift_variant() {
        assert_eq!(
            accelerator_for_class(Some("com.mitchellh.ghostty")),
            Accelerator::Terminal
        );
        assert_eq!(accelerator_for_class(Some("Alacritty")), Accelerator::Terminal);
        assert_eq!(accelerator_for_class(Some("kitty")), Accelerator::Terminal);
    }

    #[test]
    fn everything_else_gets_plain_ctrl_v() {
        assert_eq!(accelerator_for_class(Some("firefox")), Accelerator::Standard);
        assert_eq!(accelerator_for_class(Some("code")), Accelerator::Standard);
        assert_eq!(accelerator_for_class(None), Accelerator::Standard);
    }

    /// At least one injection mechanism must be usable, and the uinput path
    /// must get as far as creating its device.
    ///
    /// Deliberately stops short of emitting a keystroke: a test that pressed
    /// Ctrl+V would type into whatever window happened to hold focus on the
    /// developer's desktop. Device creation is the part that actually fails
    /// in the field — permissions, missing module — and it is silent.
    #[test]
    #[ignore = "needs a live session with wtype or /dev/uinput"]
    fn an_injection_mechanism_is_usable() {
        assert!(
            is_available(),
            "no paste mechanism available: {}",
            unavailable_reason()
        );

        if uinput_is_writable() {
            let device = build_device();
            assert!(
                device.is_ok(),
                "/dev/uinput is writable but the virtual keyboard would not build: {:?}",
                device.err()
            );
        }
    }

    /// The whole pipeline, for real: stage text on the clipboard, synthesise
    /// the accelerator, and confirm the bytes arrived inside a foreign
    /// application.
    ///
    /// Everything upstream of this can pass while the feature is still
    /// broken — the clipboard round-trip only proves we can talk to
    /// ourselves, and a `wtype` exit code of 0 only proves the process ran.
    /// Neither shows that a keystroke reached another client, which is the
    /// one thing auto-paste is for.
    ///
    /// `foot` is the target because a terminal turns a paste into observable
    /// bytes on a pipe; `head -n 1` then makes the read block until a newline
    /// arrives rather than racing the paste. Being a terminal also exercises
    /// the Ctrl+Shift+V branch, which is the one that would silently insert a
    /// control character if the accelerator choice were wrong.
    ///
    /// The explicit re-focus is not test scaffolding — it is the step
    /// `paste_final_text` performs before every paste, and skipping it sends
    /// the keystroke to whatever the compositor happened to focus instead.
    ///
    /// Requires `foot` and steals focus while it runs — so do not type while
    /// it does. Focus is pinned to the target for a couple of seconds, which
    /// means a stray keypress from the operator lands in the assertion as a
    /// prefix on the marker. That is the test being hijacked, not the paste
    /// pipeline failing.
    #[test]
    #[ignore = "needs a live Wayland session, foot, and steals focus"]
    fn pastes_into_a_foreign_window() {
        use std::io::Read as _;

        const MARKER: &str = "voicebox-e2e-paste-marker";
        const TITLE: &str = "VoiceboxPasteTarget";
        let sink = std::env::temp_dir().join("voicebox-e2e-paste.txt");
        let _ = std::fs::remove_file(&sink);

        // Pin focus to the target for the duration. Not a workaround for a
        // product bug: `input:follow_mouse` is on by default, so the pointer
        // resting over the terminal that launched `cargo test` drags focus
        // back the instant anything moves it, and the keystroke lands there
        // instead. A real user's pointer is not doing that mid-dictation.
        let _ = Command::new("hyprctl")
            .args([
                "repl",
                &format!(
                    "hl.window_rule({{ match = {{ title = [[^({TITLE})$]] }}, stay_focused = true }})"
                ),
            ])
            .status();

        let mut terminal = Command::new("foot")
            .arg(format!("--title={TITLE}"))
            .arg("--")
            .arg("sh")
            .arg("-c")
            .arg(format!("head -n 1 > {}", sink.display()))
            .spawn()
            .expect("spawn foot — install it or skip this test");

        std::thread::sleep(Duration::from_millis(1500));

        let outcome = find_window_by_title(TITLE)
            .ok_or_else(|| format!("no window titled {TITLE:?} appeared"))
            .and_then(|(address, class)| {
                // The production entry point, with the snapshot shape
                // `capture_focus` would have produced.
                crate::focus_capture::activate(&crate::focus_capture::FocusSnapshot {
                    pid: 0,
                    bundle_id: Some(class.clone()),
                    role: None,
                    window_id: Some(address),
                })?;
                std::thread::sleep(Duration::from_millis(800));
                crate::linux::clipboard::write_text(MARKER)?;
                send_paste(accelerator_for_class(Some(&class)))?;
                // `head -n 1` blocks until a newline, so the paste alone
                // never reaches the file. The gap matters: Return arriving
                // first ends the read on an empty line, which looks exactly
                // like a paste that delivered nothing.
                std::thread::sleep(Duration::from_millis(600));
                Command::new("wtype")
                    .args(["-k", "Return"])
                    .status()
                    .map(|_| ())
                    .map_err(|e| format!("could not send Return: {e}"))
            });

        let mut written = String::new();
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(mut file) = std::fs::File::open(&sink) {
                written.clear();
                let _ = file.read_to_string(&mut written);
                if !written.trim().is_empty() {
                    break;
                }
            }
        }

        let _ = terminal.kill();
        let _ = terminal.wait();
        let _ = std::fs::remove_file(&sink);

        outcome.expect("focus, stage and paste");
        assert_eq!(
            written.trim(),
            MARKER,
            "the synthesised accelerator did not deliver the clipboard into foot"
        );
    }

    /// `(address, class)` of the window with this exact title.
    fn find_window_by_title(title: &str) -> Option<(String, String)> {
        let output = Command::new("hyprctl").args(["clients", "-j"]).output().ok()?;
        let clients: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
        clients.iter().find_map(|client| {
            if client.get("title")?.as_str()? != title {
                return None;
            }
            Some((
                client.get("address")?.as_str()?.to_string(),
                client.get("class")?.as_str()?.to_string(),
            ))
        })
    }
}
