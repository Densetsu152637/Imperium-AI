//! Turns through `ratio::evaluate_with`, recorded or replayed with a `TapeHost` that the
//! evaluation also holds (`Evaluation::with_tape`). Ratio's tape is JSON lines (error answers
//! over 1 KiB as `xb` blobs, other lines at most 64 KiB); Imperium stores the text unchanged,
//! compressed, as `<turn>.jsonl.zst`, and refuses to inflate one past Ratio's tape bound.
//!
//! Before a turn Imperium pins what the program loads and starts its entry when the entry is a
//! harness, on the live host both when recording and replaying (replay still asks the live
//! host to open frames).

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use ratio::frame::Session;
use ratio::host::Host;
use ratio::tape::{MAX_TAPE_BYTES, RecordOptions, ReplayOptions, Tape, TapeHost};
use ratio::{CancelToken, Finished, Limits, Prepared, Value};

use crate::RatioHost;

/// The extension of a stored tape.
pub const TAPE_EXTENSION: &str = "jsonl.zst";

/// Writes a tape, compressed.
///
/// # Errors
///
/// When the file cannot be written.
pub fn write_tape(path: &Path, text: &str) -> io::Result<()> {
    let bytes = zstd::encode_all(text.as_bytes(), 3)?;
    std::fs::write(path, bytes)
}

/// Reads a tape written by [`write_tape`].
///
/// # Errors
///
/// When the file cannot be read or is not a compressed UTF-8 tape.
pub fn read_tape(path: &Path) -> io::Result<String> {
    let file = std::fs::File::open(path)?;
    let limit = u64::try_from(MAX_TAPE_BYTES).unwrap_or(u64::MAX);
    let mut text = Vec::new();
    zstd::Decoder::new(file)?
        .take(limit + 1)
        .read_to_end(&mut text)?;
    if text.len() > MAX_TAPE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the tape inflates past Ratio's tape bound",
        ));
    }
    String::from_utf8(text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Pins the program, starts its entry `name` of the module at `module` (when a harness), and
/// finds it.
fn prepare_turn(
    prepared: &Prepared,
    host: &RatioHost,
    module: &str,
    name: &str,
) -> Result<ratio::interp::Entry, String> {
    host.load(prepared);
    host.start_turn_entry(prepared, module, name);
    prepared
        .find(module, name)
        .ok_or_else(|| format!("no `{name}` in `{module}`"))
}

/// Runs `name` of the module at `module` once, recording its tape; returns how it finished
/// and the tape's text.
///
/// # Errors
///
/// An all-zero redaction key, or an entry that does not exist or start.
pub fn record_turn(
    prepared: Arc<Prepared>,
    (module, name): (&str, &str),
    args: Vec<Value>,
    session: &Session,
    host: Arc<RatioHost>,
    redaction_key: [u8; 32],
) -> Result<(Finished, String), String> {
    let entry = prepare_turn(&prepared, &host, module, name)?;
    let opts = RecordOptions::new(redaction_key).map_err(|e| e.to_string())?;
    let tape = Arc::new(TapeHost::record(host as Arc<dyn Host>, opts));
    let eval = ratio::evaluate_with(
        prepared,
        entry,
        args,
        Limits::default(),
        CancelToken::new(),
        session,
        Arc::clone(&tape) as Arc<dyn Host>,
    )
    .map_err(|e| format!("{e:?}"))?
    .with_tape(Arc::clone(&tape) as Arc<dyn Tape>);
    let finished = ratio::block_on(eval);
    Ok((finished, tape.tape()))
}

/// Replays `text` against `host`, verified against the tape alone. Approvals and `@user`
/// built-ins (`session.ask`) are still asked live: Ratio never answers them from a tape.
///
/// # Errors
///
/// A tape that does not load, or an entry that does not start.
pub fn replay_turn(
    prepared: Arc<Prepared>,
    (module, name): (&str, &str),
    args: Vec<Value>,
    session: &Session,
    host: Arc<RatioHost>,
    text: &str,
    redaction_key: [u8; 32],
) -> Result<Finished, String> {
    let entry = prepare_turn(&prepared, &host, module, name)?;
    let opts = ReplayOptions {
        redaction_key: Some(redaction_key),
        ..ReplayOptions::default()
    };
    let tape =
        Arc::new(TapeHost::replay(host as Arc<dyn Host>, text, opts).map_err(|e| e.to_string())?);
    let eval = ratio::evaluate_with(
        prepared,
        entry,
        args,
        Limits::default(),
        CancelToken::new(),
        session,
        Arc::clone(&tape) as Arc<dyn Host>,
    )
    .map_err(|e| format!("{e:?}"))?
    .with_tape(Arc::clone(&tape) as Arc<dyn Tape>);
    Ok(ratio::block_on(eval))
}
