//! Turns through `ratio::evaluate_with`, recorded or replayed with a `TapeHost` that the
//! evaluation also holds (`Evaluation::with_tape`). Ratio's tape is JSON lines; Imperium
//! stores it compressed, as `<turn>.jsonl.zst`.

use std::io;
use std::path::Path;
use std::sync::Arc;

use ratio::frame::Session;
use ratio::host::Host;
use ratio::interp::Entry;
use ratio::tape::{RecordOptions, ReplayOptions, Tape, TapeHost};
use ratio::{CancelToken, Finished, Limits, Prepared, Value};

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
    let bytes = std::fs::read(path)?;
    let text = zstd::decode_all(bytes.as_slice())?;
    String::from_utf8(text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Runs `entry` once, recording its tape; returns how it finished and the tape's text.
///
/// # Errors
///
/// An all-zero redaction key, or an entry that does not start.
pub fn record_turn(
    prepared: Arc<Prepared>,
    entry: Entry,
    args: Vec<Value>,
    session: &Session,
    host: Arc<dyn Host>,
    redaction_key: [u8; 32],
) -> Result<(Finished, String), String> {
    let opts = RecordOptions::new(redaction_key).map_err(|e| e.to_string())?;
    let tape = Arc::new(TapeHost::record(host, opts));
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
    entry: Entry,
    args: Vec<Value>,
    session: &Session,
    host: Arc<dyn Host>,
    text: &str,
    redaction_key: [u8; 32],
) -> Result<Finished, String> {
    let opts = ReplayOptions {
        redaction_key: Some(redaction_key),
        ..ReplayOptions::default()
    };
    let tape = Arc::new(TapeHost::replay(host, text, opts).map_err(|e| e.to_string())?);
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
