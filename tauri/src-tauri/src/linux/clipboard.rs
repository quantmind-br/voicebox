//! Wayland clipboard primitives behind the auto-paste save/restore dance.
//!
//! The X11 and macOS mental model — "write bytes into a server-owned buffer,
//! walk away" — does not exist on Wayland. A client that offers data stays
//! the source of truth for it: the compositor hands other clients a pipe and
//! the *owner* writes the bytes on demand. Owning the selection therefore
//! means keeping something alive to answer those requests.
//!
//! `wl-clipboard-rs` models exactly that. Its `copy` spawns a serving thread
//! (not a fork — important inside a Tauri process that is already
//! multi-threaded) which holds the offer until another client claims the
//! selection. Handing ownership to the next writer is what ends it, so the
//! restore at the end of the paste cycle is also what retires the thread that
//! staged the transcript.
//!
//! Reading requires the compositor to implement `wlr-data-control` or
//! `ext-data-control`. Hyprland does; [`is_available`] probes it so callers
//! can report an honest "not supported here" instead of failing mid-paste.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;

use wl_clipboard_rs::copy::{
    self, MimeSource, MimeType as CopyMimeType, Options, Source,
};
use wl_clipboard_rs::paste::{
    get_contents, get_mime_types, ClipboardType, Error as PasteError, MimeType as PasteMimeType,
    Seat,
};

/// Ceiling on a single snapshot. A clipboard holding a large image would
/// otherwise be copied into memory in full, twice (snapshot + restore offer),
/// on a path the user triggers by talking. Types are taken in the order the
/// compositor advertises them until the budget runs out, so the text payload
/// that auto-paste actually cares about is never the one dropped.
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// True when this compositor exposes a data-control protocol we can read
/// the clipboard through.
pub fn is_available() -> bool {
    !matches!(
        get_mime_types(ClipboardType::Regular, Seat::Unspecified),
        Err(PasteError::MissingProtocol { .. })
    )
}

/// Every `(mime type, bytes)` pair currently on the clipboard.
///
/// An empty clipboard yields an empty vec rather than an error — starting a
/// dictation with nothing copied is entirely normal, and the restore path
/// treats "put nothing back" correctly.
pub fn read_all() -> Result<Vec<(String, Vec<u8>)>, String> {
    let mime_types = match get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
        Ok(types) => types,
        Err(PasteError::NoSeats | PasteError::ClipboardEmpty | PasteError::NoMimeType) => {
            return Ok(Vec::new())
        }
        Err(e) => return Err(format!("could not list clipboard types: {e}")),
    };

    // HashSet iteration order is arbitrary; sort so a snapshot of the same
    // clipboard is byte-identical run to run, which keeps the change token
    // below meaningful.
    let mut mime_types: Vec<String> = mime_types.into_iter().collect();
    mime_types.sort();

    let mut items = Vec::with_capacity(mime_types.len());
    let mut budget = MAX_SNAPSHOT_BYTES;
    for mime in mime_types {
        let bytes = match read_mime(&mime) {
            Ok(bytes) => bytes,
            // One unreadable type must not cost the user the rest of their
            // clipboard — a stale offer from a client that has since exited
            // is the common case.
            Err(e) => {
                eprintln!("[voicebox] clipboard: skipping type {mime:?}: {e}");
                continue;
            }
        };
        if bytes.len() > budget {
            eprintln!(
                "[voicebox] clipboard: skipping type {mime:?} ({} bytes) — snapshot budget exhausted",
                bytes.len()
            );
            continue;
        }
        budget -= bytes.len();
        items.push((mime, bytes));
    }
    Ok(items)
}

fn read_mime(mime: &str) -> Result<Vec<u8>, String> {
    let (mut pipe, _) = get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific(mime),
    )
    .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Take ownership of the clipboard with `text` as plain text.
pub fn write_text(text: &str) -> Result<(), String> {
    let mut options = Options::new();
    options.clipboard(copy::ClipboardType::Regular);
    // wl-copy trims a single trailing newline by default. Dictation output is
    // whatever the user said, so pass it through untouched.
    options.trim_newline(false);
    options
        .copy(
            Source::Bytes(text.as_bytes().into()),
            CopyMimeType::Text,
        )
        .map_err(|e| format!("could not write clipboard text: {e}"))
}

/// Re-offer a previously captured set of `(mime type, bytes)` pairs.
pub fn write_all(items: &[(String, Vec<u8>)]) -> Result<(), String> {
    if items.is_empty() {
        // Restoring "nothing" means the user had an empty clipboard before
        // dictation. Clearing is the faithful restore; leaving our transcript
        // there would be the clipboard-stomp this whole path exists to avoid.
        return copy::clear(copy::ClipboardType::Regular, copy::Seat::All)
            .map_err(|e| format!("could not clear clipboard: {e}"));
    }

    let sources = items
        .iter()
        .map(|(mime, bytes)| MimeSource {
            source: Source::Bytes(bytes.clone().into_boxed_slice()),
            mime_type: CopyMimeType::Specific(mime.clone()),
        })
        .collect();

    let mut options = Options::new();
    options.clipboard(copy::ClipboardType::Regular);
    options.trim_newline(false);
    // The snapshot already carries every text alias the original owner
    // advertised; letting wl-clipboard-rs add its own would offer types the
    // source app never did.
    options.omit_additional_text_mime_types(true);
    options
        .copy_multi(sources)
        .map_err(|e| format!("could not restore clipboard: {e}"))
}

/// A cheap stand-in for `NSPasteboard.changeCount`.
///
/// Wayland exposes no monotonic clipboard generation counter, so there is
/// nothing to read that says "someone else wrote here". What the caller
/// actually needs is narrower than a counter: between staging the transcript
/// and restoring, did the clipboard stop being ours? Hashing the current
/// contents answers that. Identical content hashing equal is not a false
/// negative — restoring content byte-identical to what is already there is a
/// no-op either way.
pub fn content_token() -> Result<i64, String> {
    let mime_types = match get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
        Ok(types) => types,
        Err(PasteError::NoSeats | PasteError::ClipboardEmpty | PasteError::NoMimeType) => {
            return Ok(0)
        }
        Err(e) => return Err(format!("could not list clipboard types: {e}")),
    };

    let mut sorted: Vec<String> = mime_types.into_iter().collect();
    sorted.sort();

    let mut hasher = DefaultHasher::new();
    sorted.hash(&mut hasher);
    // Hash the text payload too, so replacing one snippet with another of the
    // same shape still reads as a change.
    if let Ok((mut pipe, _)) = get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Text,
    ) {
        let mut bytes = Vec::new();
        if pipe.read_to_end(&mut bytes).is_ok() {
            bytes.hash(&mut hasher);
        }
    }
    Ok(hasher.finish() as i64)
}
