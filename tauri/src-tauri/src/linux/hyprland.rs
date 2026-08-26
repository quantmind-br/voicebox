//! Hyprland IPC — the compositor-side half of the Linux auto-paste and
//! overlay support.
//!
//! Wayland deliberately gives clients no way to ask "who has focus?", to
//! raise another client, or to place their own toplevel. Those are all
//! compositor privileges, so the only portable answer on Wayland is "you
//! can't". Every compositor that *does* expose them does so through its own
//! private channel; Hyprland's is a pair of unix sockets under
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`.
//!
//! Two transports are used here, deliberately:
//!
//!   - **Queries** go straight down `.socket.sock` as `j/<request>`. That
//!     wire format has been stable across every Hyprland release that
//!     matters and costs no process spawn, which is what we want on the
//!     chord-start path.
//!   - **Mutations** (dispatch, window rules) shell out to `hyprctl`. Their
//!     syntax is *not* stable — Hyprland 0.56 moved config and dispatch onto
//!     a Lua runtime, so `keyword` disappeared and `dispatch` started parsing
//!     its argument as Lua. `hyprctl` ships with the compositor and is
//!     therefore always in step with it, which buys forward-compatibility for
//!     the price of a ~3 ms spawn on a path that runs once per utterance.
//!
//! [`Capabilities`] resolves which mutation dialect this compositor speaks,
//! once, on first use.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// Requests are answered promptly or not at all — a hung compositor must not
/// wedge the dictation pipeline behind it.
const IPC_TIMEOUT: Duration = Duration::from_millis(500);

// ========================================================================
// Session detection
// ========================================================================

/// True when this process is running under a Hyprland session we can talk to.
pub fn is_active() -> bool {
    socket_path().is_some()
}

fn socket_path() -> Option<PathBuf> {
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let path = PathBuf::from(runtime)
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    path.exists().then_some(path)
}

// ========================================================================
// Query transport
// ========================================================================

/// Send one request down the command socket and read the reply to EOF.
/// Hyprland closes the connection when it's done writing, so "read to end"
/// is the framing.
fn request(payload: &str) -> Result<String, String> {
    let path = socket_path().ok_or("not running under Hyprland")?;
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("Hyprland IPC connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(IPC_TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(IPC_TIMEOUT)))
        .map_err(|e| format!("Hyprland IPC timeout setup failed: {e}"))?;
    stream
        .write_all(payload.as_bytes())
        .map_err(|e| format!("Hyprland IPC write failed: {e}"))?;
    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .map_err(|e| format!("Hyprland IPC read failed: {e}"))?;
    Ok(reply)
}

// ========================================================================
// Query types
// ========================================================================

/// The subset of `hyprctl activewindow -j` the paste pipeline consumes.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ActiveWindow {
    /// Hyprland's own window handle, e.g. `"0x5647a06fce20"`. More precise
    /// than the PID for re-focusing, since one process can own many windows.
    pub address: String,
    pub pid: i32,
    pub class: String,
    pub title: String,
}

/// The window that had keyboard focus at the moment of the call.
///
/// Returns `Ok(None)` — not an error — when nothing is focused. An empty
/// desktop is a legitimate state, and the caller treats "no target" very
/// differently from "the IPC broke".
pub fn active_window() -> Result<Option<ActiveWindow>, String> {
    let raw = request("j/activewindow")?;
    // Hyprland answers `{}` when no window is focused, which deserialises
    // into a missing-field error rather than a null. Check before parsing.
    if raw.trim() == "{}" || raw.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| format!("Hyprland IPC: could not parse `activewindow` reply: {e}"))
}

// ========================================================================
// Mutation transport
// ========================================================================

/// Which dialect this compositor's `hyprctl` speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// Hyprland 0.56+ — config and dispatch run on a Lua runtime, `keyword`
    /// is gone, and window rules are registered via `hl.window_rule{...}`.
    Lua,
    /// Hyprland ≤ 0.55 — `hyprctl keyword windowrulev2 …`, `hyprctl dispatch
    /// focuswindow pid:N`.
    Legacy,
}

fn dialect() -> Dialect {
    static DIALECT: OnceLock<Dialect> = OnceLock::new();
    *DIALECT.get_or_init(|| {
        // `repl` only exists on the Lua builds, and `hl.window_rule` is the
        // specific entry point we depend on — probe for it rather than for a
        // version number, so a rename shows up as a clean fallback instead of
        // silently no-oping every rule we register.
        let probe = Command::new("hyprctl")
            .args(["repl", "return type(hl.window_rule)"])
            .output();
        match probe {
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "function" => Dialect::Lua,
            _ => Dialect::Legacy,
        }
    })
}

fn hyprctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("hyprctl")
        .args(args)
        .output()
        .map_err(|e| format!("hyprctl {} failed to spawn: {e}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // hyprctl exits 0 even for rejected requests and reports the problem on
    // stdout, so the exit status alone is not a success signal.
    if !out.status.success() || stdout.starts_with("error:") || stdout == "unknown request" {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "hyprctl {} rejected: {}",
            args.join(" "),
            if stdout.is_empty() { stderr.trim() } else { &stdout }
        ));
    }
    Ok(stdout)
}

/// Raise and focus the window with the given Hyprland address.
///
/// Address beats PID here: a browser or editor with several windows has one
/// PID for all of them, and `pid:` matching would pick an arbitrary one.
pub fn focus_address(address: &str) -> Result<(), String> {
    // Addresses come straight back from our own `activewindow` query, so
    // they're compositor-issued hex handles rather than user input. Validate
    // anyway — this string is interpolated into a Lua expression below.
    if !address.starts_with("0x") || !address[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("refusing to dispatch on malformed address {address:?}"));
    }
    match dialect() {
        // `hl.dsp.focus{…}` only *builds* a dispatcher; handing it to
        // `hl.dispatch` is what runs it. Calling the builder alone succeeds
        // silently and does nothing, which is a quiet way to lose a feature.
        Dialect::Lua => hyprctl(&[
            "repl",
            &format!(
                "hl.dispatch(hl.dsp.focus({{ window = {selector} }}))",
                selector = lua_string(&format!("address:{address}")),
            ),
        ]),
        Dialect::Legacy => hyprctl(&["dispatch", "focuswindow", &format!("address:{address}")]),
    }
    .map(|_| ())
}

/// Pin the dictation pill into an overlay-like role.
///
/// None of this is expressible through xdg-shell: `always_on_top`,
/// `visible_on_all_workspaces`, `skip_taskbar` and `set_position` are all
/// X11-era GTK calls that Wayland turns into no-ops, so a Tauri window that
/// asks for them gets an ordinary tiled window instead of an overlay. The
/// compositor-native equivalent is a window rule, which we register for our
/// own title at startup so the user does not have to paste anything into
/// their config.
///
/// `no_focus` is the load-bearing one. The rest is polish; without it,
/// Hyprland focuses the pill the moment it maps and yanks the keyboard out
/// of whatever the user was dictating into — which would break dictation
/// even before the paste lands.
///
/// Placement rides along as a rule rather than a separate dispatch. That is
/// not just tidier: `window.move` acts on the *focused* window, and the pill
/// is precisely the window that will never be focused. Hyprland's percentage
/// syntax also resolves against whichever monitor the pill maps on, so
/// `50%- 4%` follows the user between displays with no scale arithmetic on
/// our side — `50%-` means "half the monitor, less half the window", i.e.
/// centred.
pub fn apply_overlay_rules(title_regex: &str) -> Result<(), String> {
    const PLACEMENT: &str = "50%- 4%";

    match dialect() {
        Dialect::Lua => {
            let lua = format!(
                "hl.window_rule({{ match = {{ title = {title} }}, \
                 float = true, pin = true, no_focus = true, no_border = true, \
                 no_shadow = true, no_blur = true, no_anim = true, move = {placement} }})",
                title = lua_string(title_regex),
                placement = lua_string(PLACEMENT),
            );
            hyprctl(&["repl", &lua]).map(|_| ())
        }
        Dialect::Legacy => {
            let placement = format!("move {PLACEMENT}");
            let mut failures = Vec::new();
            for rule in [
                "float",
                "pin",
                "nofocus",
                "noborder",
                "noshadow",
                "noblur",
                "noanim",
                placement.as_str(),
            ] {
                let value = format!("{rule},title:{title_regex}");
                if let Err(e) = hyprctl(&["keyword", "windowrulev2", &value]) {
                    failures.push(e);
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures.join("; "))
            }
        }
    }
}

/// Render `value` as a Lua string literal.
///
/// Window-rule regexes contain backslashes (`\\.` to escape a literal dot in
/// a class name) which Lua's `"…"` syntax would eat as escape sequences. Lua's
/// long-bracket form has no escapes at all, so it round-trips a regex
/// verbatim; the `=` padding grows until the delimiter cannot appear in the
/// payload.
fn lua_string(value: &str) -> String {
    let mut level = 0;
    while value.contains(&format!("]{}]", "=".repeat(level))) {
        level += 1;
    }
    let pad = "=".repeat(level);
    format!("[{pad}[{value}]{pad}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_string_leaves_regex_escapes_intact() {
        assert_eq!(lua_string(r"^(foo\.bar)$"), r"[[^(foo\.bar)$]]");
    }

    #[test]
    fn lua_string_grows_delimiter_past_collisions() {
        assert_eq!(lua_string("a]]b"), "[=[a]]b]=]");
        assert_eq!(lua_string("a]]b]=]c"), "[==[a]]b]=]c]==]");
    }

}
