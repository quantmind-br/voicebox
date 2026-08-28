//! Global hotkey → dictation effect bridge.
//!
//! Thin adapter from `keytap::chord::ChordMatcher` to Tauri events. keytap
//! owns the OS event tap + the chord state machine (Momentary vs Toggle,
//! longest-match resolution, sticky-toggle semantics); this module's only
//! job is:
//!
//!   1. Build a `ChordMatcher` from the user's saved PTT + Toggle chords.
//!   2. Translate `ChordEvent` → voicebox's [`Effect`] on a dispatcher
//!      thread.
//!   3. Fan [`Effect`]s out into Tauri events + dictate-window show/hide.
//!
//! The [`Effect::RestartRecording`] signal is emitted when keytap fires
//! `End(PTT)` and `Start(Toggle)` with the *same* [`Instant`] — which
//! happens when the held set upgrades from a shorter chord to a longer
//! superset in a single event (the classic PTT→hands-free transition).
//! We detect the pair with a 5 ms peek on the matcher's receiver and
//! coalesce into one `Restart` so hosts can discard the transition-
//! moment audio rather than treat it as an unrelated Stop+Start pair.
//!
//! Escape-to-cancel rides on a *second*, short-lived [`Tap`] armed only
//! while a dictation is running — see [`arm_escape_cancel`] for why it
//! can't be another chord on the shared matcher.
//!
//! Left- and right-hand modifier variants are kept distinct all the way
//! down to the OS event tap (keytap's core promise). Defaults bind to
//! right-hand Cmd + right-hand Option on macOS / right-hand Ctrl +
//! right-hand Shift on Windows so the usual left-hand shortcuts stay
//! with the OS / app.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use keytap::chord::{Chord, ChordEvent, ChordMatcher};
use keytap::{EventKind, Key, RecvTimeoutError, Tap};
use tauri::{AppHandle, Emitter, Manager};

use crate::focus_capture;
use crate::DICTATE_WINDOW_LABEL;

// ========================================================================
// Public types
// ========================================================================

/// Semantic action a chord can be bound to. `PushToTalk` = hold chord to
/// record, release to stop. `ToggleToTalk` = press chord to start recording,
/// press again to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordAction {
    PushToTalk,
    ToggleToTalk,
}

/// Effect produced after the chord matcher resolves an event. Hosts
/// translate these into UI / recorder calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    StartRecording(ChordAction),
    StopRecording(ChordAction),
    /// Emitted when a push-to-talk chord is "upgraded" into the toggle
    /// chord mid-hold — hosts may want to discard the captured audio and
    /// restart so the transition moment isn't in the recording.
    RestartRecording(ChordAction),
}

/// Chord key sets from capture settings. Both actions use the same
/// `HashSet<Key>` shape so callers don't need to know about keytap's
/// `Chord` type.
pub type Bindings = HashMap<ChordAction, HashSet<Key>>;

// ========================================================================
// Monitor
// ========================================================================

pub struct HotkeyMonitor {
    app: AppHandle,
    /// Last bindings handed to [`Self::apply`], kept so [`Self::reset`] can
    /// rebuild the matcher without the caller having to re-read settings.
    bindings: Bindings,
    active: Option<Active>,
}

struct Active {
    dispatcher: JoinHandle<()>,
    shutdown: Arc<AtomicBool>,
}

impl HotkeyMonitor {
    /// Build the monitor with initial bindings. Equivalent to constructing
    /// an empty monitor and calling [`Self::update_bindings`] once.
    pub fn spawn(app: AppHandle, bindings: Bindings) -> Self {
        let mut m = Self {
            app,
            bindings: Bindings::new(),
            active: None,
        };
        m.apply(bindings);
        m
    }

    /// Swap in a fresh set of chord bindings. Tears down the existing
    /// `ChordMatcher` (which stops keytap's chord worker thread and
    /// closes the OS tap) and spawns a new one. No-op for the "all
    /// empty" case so "disable hotkey" doesn't keep a tap running for
    /// no reason.
    pub fn update_bindings(&mut self, bindings: Bindings) {
        self.apply(bindings);
    }

    /// Rebuild the matcher from the bindings already in force, discarding
    /// whatever chord state it had accumulated.
    ///
    /// Needed after Escape-to-cancel ends a recording behind keytap's back.
    /// A toggle session is the case that bites: the matcher still considers
    /// the toggle chord active, so the user's next press reads as that
    /// session's "off" press instead of starting a fresh one — and while it
    /// thinks a Toggle is active, keytap suppresses push-to-talk entirely.
    /// A rebuild puts the matcher and the user back in sync.
    ///
    /// Must not be called from the dispatcher thread or the Escape watcher:
    /// [`Self::apply`] joins both. The `reset_chord_state` command is
    /// declared `async` so Tauri runs it off the main thread, outside that
    /// pair.
    pub fn reset(&mut self) {
        let bindings = self.bindings.clone();
        self.apply(bindings);
    }

    fn apply(&mut self, bindings: Bindings) {
        // The Escape watcher belongs to the monitor session being torn down:
        // once the dispatcher is gone, nothing will ever emit the
        // `StopRecording` that would otherwise disarm it, so it would outlive
        // every capture and keep an OS tap open for the rest of the process.
        // This is the path the reset after an Escape-cancel takes; `Drop` does
        // the same for itself. Safe to interleave with the dispatcher's own disarm:
        // `disarm_escape_cancel` never holds the mutex across a join, so the
        // dispatcher can't be stuck on it while we wait to join the dispatcher.
        disarm_escape_cancel();

        // Tear down any existing matcher + dispatcher first. The
        // dispatcher sees the shutdown flag on its next recv_timeout
        // (≤100ms) and returns; joining waits for that. Dropping the
        // ChordMatcher stops keytap's chord-worker thread and the
        // underlying Tap.
        if let Some(active) = self.active.take() {
            active.shutdown.store(true, Ordering::Relaxed);
            let _ = active.dispatcher.join();
        }

        self.bindings = bindings;

        if self.bindings.values().all(|set| set.is_empty()) {
            return;
        }

        let matcher = match build_matcher(&self.bindings) {
            Ok(m) => m,
            Err(err) => {
                eprintln!(
                    "HotkeyMonitor: ChordMatcher build failed ({err}). Global chord detection is disabled. On macOS, grant Input Monitoring in System Settings → Privacy & Security → Input Monitoring and relaunch."
                );
                return;
            }
        };

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = shutdown.clone();
        let app = self.app.clone();
        let dispatcher = thread::Builder::new()
            .name("voicebox-hotkey-dispatcher".into())
            .spawn(move || dispatcher_loop(app, matcher, shutdown_for_thread))
            .expect("spawn hotkey dispatcher thread");

        self.active = Some(Active {
            dispatcher,
            shutdown,
        });
    }
}

impl Drop for HotkeyMonitor {
    fn drop(&mut self) {
        // Same reason `apply` does it: nothing left alive would ever emit the
        // `StopRecording` that disarms the watcher.
        disarm_escape_cancel();
        if let Some(active) = self.active.take() {
            active.shutdown.store(true, Ordering::Relaxed);
            let _ = active.dispatcher.join();
        }
    }
}

// ========================================================================
// Matcher construction + dispatch
// ========================================================================

fn build_matcher(bindings: &Bindings) -> Result<ChordMatcher<ChordAction>, keytap::Error> {
    let mut builder = ChordMatcher::builder();
    if let Some(keys) = bindings.get(&ChordAction::PushToTalk) {
        if !keys.is_empty() {
            builder = builder.add(ChordAction::PushToTalk, Chord::of(keys.iter().copied()));
        }
    }
    if let Some(keys) = bindings.get(&ChordAction::ToggleToTalk) {
        if !keys.is_empty() {
            builder =
                builder.add_toggle(ChordAction::ToggleToTalk, Chord::of(keys.iter().copied()));
        }
    }
    builder.build()
}

fn dispatcher_loop(app: AppHandle, matcher: ChordMatcher<ChordAction>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        match matcher.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => process_event(&app, &matcher, event),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Turn a single [`ChordEvent`] into zero or one [`Effect`]s, peeking at
/// the matcher once for a same-Instant follow-up so upgrade transitions
/// coalesce into [`Effect::RestartRecording`] instead of a Stop+Start
/// pair.
fn process_event(
    app: &AppHandle,
    matcher: &ChordMatcher<ChordAction>,
    event: ChordEvent<ChordAction>,
) {
    match event {
        ChordEvent::Start { id, .. } => {
            apply_effect(app, Effect::StartRecording(id));
        }
        ChordEvent::End {
            id: end_id,
            time: end_time,
        } => {
            // Peek for an immediately-following Start. keytap emits
            // End+Start atomically (same Instant) when the held set
            // transitions between registered chords — our 5 ms window
            // is well under perceptible latency but far longer than the
            // channel hop between keytap's chord worker and our
            // dispatcher.
            match matcher.recv_timeout(Duration::from_millis(5)) {
                Ok(ChordEvent::Start {
                    id: start_id,
                    time: start_time,
                }) if start_time == end_time => {
                    apply_effect(app, Effect::RestartRecording(start_id));
                }
                Ok(other) => {
                    apply_effect(app, Effect::StopRecording(end_id));
                    // The peeked event wasn't a transition partner;
                    // process it in its own right. Recursion depth is
                    // bounded by the number of back-to-back chord
                    // events, in practice 1–2.
                    process_event(app, matcher, other);
                }
                Err(_) => {
                    apply_effect(app, Effect::StopRecording(end_id));
                }
            }
        }
    }
}

// ========================================================================
// Escape-to-cancel
// ========================================================================

/// The Escape watcher currently armed, if any. Touched from the dispatcher
/// thread (chord start/stop) and from whichever thread runs
/// [`HotkeyMonitor::apply`], hence the mutex.
static ESCAPE_WATCH: Mutex<Option<EscapeWatch>> = Mutex::new(None);

struct EscapeWatch {
    thread: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for EscapeWatch {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start watching for a global Escape press, emitting `dictate:cancel` at
/// the pill when one lands. No-op when already armed, so a PTT→toggle
/// upgrade keeps the same tap instead of churning it.
///
/// Escape gets its own [`Tap`] rather than a third chord on the shared
/// [`ChordMatcher`] because keytap suppresses every other registered chord
/// while a Toggle chord is active — an Escape chord would go silent during
/// exactly the hands-free sessions where cancelling matters most. Taps are
/// read-only (evdev reads / a listen-only event tap), so Escape still
/// reaches whatever app the user was typing in.
///
/// Armed only for the duration of a capture: outside a dictation there is
/// no second tap open at all.
pub fn arm_escape_cancel(app: &AppHandle) {
    let mut slot = ESCAPE_WATCH.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_some() {
        return;
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_thread = shutdown.clone();
    let app = app.clone();
    // Degrade the way the `Tap::new` failure below does. Panicking here would
    // take down the dispatcher thread — which is what calls this — and poison
    // the mutex, trading "no Esc-to-cancel" for "no global hotkey at all".
    let thread = match thread::Builder::new()
        .name("voicebox-escape-watch".into())
        .spawn(move || escape_loop(app, shutdown_for_thread))
    {
        Ok(thread) => thread,
        Err(err) => {
            eprintln!(
                "HotkeyMonitor: could not spawn the Escape watch thread ({err}). Esc-to-cancel is unavailable for this capture."
            );
            return;
        }
    };

    *slot = Some(EscapeWatch {
        thread: Some(thread),
        shutdown,
    });
}

/// Stop watching. Safe to call when nothing is armed.
///
/// Reached from the dispatcher on chord release and from
/// [`HotkeyMonitor::apply`] — both ordered against the arm, so a stale
/// disarm can never strip a freshly-started capture of its watcher.
///
/// The watcher is taken out of the slot and dropped *after* the guard is
/// released: `EscapeWatch::drop` joins a thread that can be parked up to
/// 100 ms in `recv_timeout`, and holding the mutex across that would stall
/// chord dispatch and any concurrent arm.
pub fn disarm_escape_cancel() {
    let watch = ESCAPE_WATCH
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    drop(watch);
}

fn escape_loop(app: AppHandle, shutdown: Arc<AtomicBool>) {
    let tap = match Tap::new() {
        Ok(tap) => tap,
        Err(err) => {
            eprintln!(
                "HotkeyMonitor: Escape tap failed ({err}). Esc-to-cancel is unavailable for this capture."
            );
            return;
        }
    };

    // One cancel is all a capture can use. Without this, mashing Escape
    // lands several `dictate:cancel`s before React has settled, and each one
    // that slips past the pill's guard costs a full matcher teardown +
    // rebuild. Keep looping rather than returning, so the slot still holds a
    // live watcher for the disarm to take.
    let mut cancelled = false;

    while !shutdown.load(Ordering::Relaxed) {
        match tap.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if !cancelled && matches!(event.kind, EventKind::KeyDown(Key::Escape)) {
                    if let Some(window) = app.get_webview_window(DICTATE_WINDOW_LABEL) {
                        let _ = window.emit("dictate:cancel", ());
                    }
                    cancelled = true;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

// ========================================================================
// Effect → Tauri
// ========================================================================

fn apply_effect(app: &AppHandle, effect: Effect) {
    match effect {
        Effect::StartRecording(_) => {
            // Snapshot focus BEFORE we touch the window — any AppKit
            // reshuffle triggered by set_position / show could in principle
            // steal key focus and poison the reading. In practice those
            // calls leave keyWindow alone, but capturing first is free.
            let focus = focus_capture::capture_focus().ok();

            arm_escape_cancel(app);

            if let Some(window) = app.get_webview_window(DICTATE_WINDOW_LABEL) {
                // The previous hide-cycle parked the window off-screen and
                // made it click-through — undo both before showing, so the
                // pill lands at top-center and the user can actually click
                // the error pill / stop button.
                //
                // Skip on Linux: aborts if the window was never realized
                // (see show_dictate_window in main.rs).
                #[cfg(not(target_os = "linux"))]
                let _ = window.set_ignore_cursor_events(false);
                // Deliberately no set_focus() — taking key focus would yank
                // it out of whatever app the user was typing in, which is
                // the opposite of what a dictation overlay should do. On
                // Wayland the same guarantee comes from the `no_focus` window
                // rule, since the compositor focuses newly mapped windows by
                // default and does not ask the client's opinion.
                #[cfg(not(target_os = "linux"))]
                crate::position_dictate_window(&window);
                #[cfg(target_os = "linux")]
                crate::refresh_dictate_overlay_rules();
                let _ = window.show();
                let payload = serde_json::json!({ "focus": focus });
                let _ = window.emit("dictate:start", payload);
            }
        }
        Effect::StopRecording(_) => {
            disarm_escape_cancel();
            if let Some(window) = app.get_webview_window(DICTATE_WINDOW_LABEL) {
                let _ = window.emit("dictate:stop", ());
            }
        }
        Effect::RestartRecording(_) => {
            // Already armed by the Start this restart replaces; the call is
            // a no-op, and is here so a Restart that somehow arrives first
            // still gets a watcher.
            arm_escape_cancel(app);
            if let Some(window) = app.get_webview_window(DICTATE_WINDOW_LABEL) {
                let _ = window.emit("dictate:restart", ());
            }
        }
    }
}
