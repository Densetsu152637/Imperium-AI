//! NCM weight files (Princeps), served by `ncm-server`: recognising one by its safetensors header
//! and sizing what it needs.
//!
//! An NCM file is one `.safetensors` file whose header `__metadata__` holds
//! `ncm_format = "ncm-safetensors-v1"` and `ncm_config` (the model config as TOML), with
//! `tokenizer.json` beside it. It is recognised by that marker only, never by its name. The
//! header is untrusted input: at most [`MAX_HEADER_BYTES`] of it is read, every field has a size
//! limit, and every number taken from the config is checked against a range; a file that fails
//! is listed with a problem rather than failing the scan or producing a footprint.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The `ncm_format` value this runtime reads.
pub const NCM_FORMAT: &str = "ncm-safetensors-v1";

/// Most bytes of JSON header read while scanning. An NCM header holds a few hundred tensor
/// entries, far below this; the safetensors format itself allows 100 MiB.
pub const MAX_HEADER_BYTES: u64 = 1024 * 1024;

/// Longest `ncm_config` read, in bytes.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// Longest `ncm_quant` read, in characters.
pub const MAX_QUANT_CHARS: usize = 64;

/// The longest context an NCM model is built for (1M tokens): the ceiling `auto` caps to.
pub const NCM_MAX_CONTEXT: u64 = 1_048_576;

/// The marker looked for in a header that cannot be parsed, so a damaged NCM file is listed
/// with its problem while any other safetensors file stays unlisted.
const MARKER: &[u8] = b"\"ncm_format\"";

/// Cache bytes per token in bf16, from the NCM session's published sizes at 1M tokens
/// (`ncm_model::cache_bytes`: about 1 GiB + 12 MiB for the NCM and 5 GiB + 60 MiB for the token
/// baseline, 250M configs), rounded up by about 6 %.
const NCM_CACHE_BYTES_PER_TOKEN_BF16: u64 = 1_100;
const TOK_CACHE_BYTES_PER_TOKEN_BF16: u64 = 5_500;

/// Cache state that does not grow with the context (DeltaNet states, the attention rings).
const FIXED_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// Which of the two architectures `ncm-server` runs a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NcmArch {
    /// The Next Concept Model (`model = "ncm"`).
    Ncm,
    /// The token baseline (`model = "tok"`).
    Tok,
}

impl NcmArch {
    pub fn name(self) -> &'static str {
        match self {
            NcmArch::Ncm => "ncm",
            NcmArch::Tok => "tok",
        }
    }
}

/// What the header's `ncm_config` says about the model, every number within its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NcmShape {
    pub arch: NcmArch,
    pub vocab_size: u32,
    /// Backbone layers (`bb_layers` of the NCM, `n_layers` of the token baseline).
    pub layers: u32,
    /// Backbone width (`bb_dim`, `d_model`).
    pub width: u32,
    /// Tokens per concept (`k`); 1 for the token baseline.
    pub k: u32,
    /// `ncm_quant`, when the file is quantised (`q8`, `q4-g64`, ...).
    pub quant: Option<String>,
}

/// What a `.safetensors` file's header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    /// No NCM marker: an ordinary safetensors file, which the scan leaves alone.
    NotNcm,
    /// A readable NCM file.
    Ncm(NcmShape),
    /// A file with the NCM marker that cannot be used, and why.
    Problem(String),
}

/// Reads the header of the safetensors file at `path` (never more than [`MAX_HEADER_BYTES`] of
/// it). Errors reading the file are [`Header::NotNcm`]: nothing says it is an NCM file.
pub fn read_header(path: &Path) -> Header {
    match File::open(path) {
        Ok(mut file) => read_header_from(&mut file),
        Err(_) => Header::NotNcm,
    }
}

/// [`read_header`] over an open file positioned anywhere.
pub fn read_header_from(file: &mut (impl Read + Seek)) -> Header {
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return Header::NotNcm;
    };
    if len < 8 || file.seek(SeekFrom::Start(0)).is_err() {
        return Header::NotNcm;
    }
    let mut size = [0; 8];
    if file.read_exact(&mut size).is_err() {
        return Header::NotNcm;
    }
    let declared = u64::from_le_bytes(size);
    let available = len - 8;
    // What may be read: the declared header, capped by the file and the scan's limit.
    let readable = declared.min(available).min(MAX_HEADER_BYTES);
    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take(readable)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Header::NotNcm;
    }
    let marked = bytes.windows(MARKER.len()).any(|window| window == MARKER);
    if declared > MAX_HEADER_BYTES {
        return if marked {
            Header::Problem(format!(
                "its header is too large to read ({declared} bytes; at most {MAX_HEADER_BYTES})"
            ))
        } else {
            Header::NotNcm
        };
    }
    if declared > available {
        return if marked {
            Header::Problem("its header runs past the end of the file".into())
        } else {
            Header::NotNcm
        };
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return if marked {
            Header::Problem("its header is not valid JSON".into())
        } else {
            Header::NotNcm
        };
    };
    parse_metadata(json.get("__metadata__"))
}

/// The NCM fields of a header's `__metadata__`.
fn parse_metadata(metadata: Option<&serde_json::Value>) -> Header {
    let Some(metadata) = metadata.and_then(serde_json::Value::as_object) else {
        return Header::NotNcm;
    };
    let Some(format) = metadata.get("ncm_format") else {
        return Header::NotNcm;
    };
    match format.as_str() {
        Some(NCM_FORMAT) => {}
        Some(other) => {
            let shown: String = other.chars().take(64).collect();
            return Header::Problem(format!(
                "its NCM format {shown:?} is not one this Imperium reads ({NCM_FORMAT})"
            ));
        }
        None => return Header::Problem("its ncm_format is not a string".into()),
    }
    let Some(config) = metadata
        .get("ncm_config")
        .and_then(serde_json::Value::as_str)
    else {
        return Header::Problem("its header has no ncm_config".into());
    };
    if config.len() > MAX_CONFIG_BYTES {
        return Header::Problem(format!(
            "its ncm_config is longer than {MAX_CONFIG_BYTES} bytes"
        ));
    }
    let quant = match metadata.get("ncm_quant") {
        None => None,
        Some(serde_json::Value::String(quant))
            if quant.chars().count() <= MAX_QUANT_CHARS
                && quant
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) =>
        {
            Some(quant.clone())
        }
        Some(_) => return Header::Problem("its ncm_quant is not a short quantisation name".into()),
    };
    match parse_config(config) {
        Ok(mut shape) => {
            shape.quant = quant;
            Header::Ncm(shape)
        }
        Err(problem) => Header::Problem(format!("its ncm_config: {problem}")),
    }
}

/// Reads the shape from `ncm_config`, refusing a number outside its range.
pub fn parse_config(config: &str) -> Result<NcmShape, String> {
    let table: toml::Table = toml::from_str(config).map_err(|_| "is not valid TOML".to_string())?;
    let number = |key: &str, range: std::ops::RangeInclusive<u32>| -> Result<u32, String> {
        let value = table
            .get(key)
            .ok_or_else(|| format!("{key} is missing"))?
            .as_integer()
            .ok_or_else(|| format!("{key} is not a whole number"))?;
        u32::try_from(value)
            .ok()
            .filter(|value| range.contains(value))
            .ok_or_else(|| {
                format!(
                    "{key} = {value} is outside {}..={}",
                    range.start(),
                    range.end()
                )
            })
    };
    let vocab = 1..=1_048_576;
    let layers = 1..=512;
    let width = 1..=65_536;
    match table.get("model").and_then(toml::Value::as_str) {
        Some("ncm") => Ok(NcmShape {
            arch: NcmArch::Ncm,
            vocab_size: number("vocab_size", vocab)?,
            layers: number("bb_layers", layers)?,
            width: number("bb_dim", width.clone())?,
            k: number("k", 1..=64)?,
            quant: None,
        }),
        Some("tok") => Ok(NcmShape {
            arch: NcmArch::Tok,
            vocab_size: number("vocab_size", vocab)?,
            layers: number("n_layers", layers)?,
            width: number("d_model", width)?,
            k: 1,
            quant: None,
        }),
        Some(other) => {
            let shown: String = other.chars().take(32).collect();
            Err(format!("model {shown:?} is neither ncm nor tok"))
        }
        None => Err("model is missing".into()),
    }
}

/// Bytes of cache `ncm-server` allocates for `tokens` of context with values of `itemsize`
/// bytes (2 for bf16 on CUDA, 4 for f32 on the CPU); `None` on overflow.
pub fn cache_bytes(shape: &NcmShape, tokens: u64, itemsize: u64) -> Option<u64> {
    let per_token_bf16 = match shape.arch {
        NcmArch::Ncm => NCM_CACHE_BYTES_PER_TOKEN_BF16,
        NcmArch::Tok => TOK_CACHE_BYTES_PER_TOKEN_BF16,
    };
    let per_token = per_token_bf16.checked_mul(itemsize)?.div_ceil(2);
    per_token
        .checked_mul(tokens)?
        .checked_add(FIXED_CACHE_BYTES)
}

/// Why the NCM file at `path` cannot load without anything beside it, if it cannot:
/// `ncm-server` reads `tokenizer.json` from the weights' folder.
pub fn missing_tokenizer(path: &Path) -> Option<String> {
    let tokenizer = path.with_file_name("tokenizer.json");
    (!tokenizer.is_file()).then(|| {
        format!(
            "no tokenizer.json beside it (looked for {}); ncm-server needs it",
            tokenizer.display()
        )
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    /// A safetensors file of one tiny tensor with `metadata` in its header.
    pub(crate) fn file(metadata: &serde_json::Value) -> Vec<u8> {
        let header = serde_json::json!({
            "__metadata__": metadata,
            "embed.weight": {"dtype": "BF16", "shape": [2, 2], "data_offsets": [0, 8]},
        })
        .to_string();
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend(header.as_bytes());
        bytes.extend([0; 8]);
        bytes
    }

    pub(crate) const NCM_CONFIG: &str = "model = \"ncm\"\nattention = \"v2\"\nvocab_size = 32000\nk = 4\nlocal_dim = 512\nbb_dim = 1024\nbb_layers = 16\n";

    /// An NCM file with the 250M v2 shape.
    pub(crate) fn ncm() -> Vec<u8> {
        file(&serde_json::json!({
            "ncm_format": super::NCM_FORMAT,
            "ncm_config": NCM_CONFIG,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn header(bytes: Vec<u8>) -> Header {
        read_header_from(&mut Cursor::new(bytes))
    }

    #[test]
    fn a_marked_file_is_read_with_its_shape() {
        let Header::Ncm(shape) = header(fixture::ncm()) else {
            panic!("not read as NCM");
        };
        assert_eq!(
            shape,
            NcmShape {
                arch: NcmArch::Ncm,
                vocab_size: 32000,
                layers: 16,
                width: 1024,
                k: 4,
                quant: None,
            }
        );
        let quantised = fixture::file(&serde_json::json!({
            "ncm_format": NCM_FORMAT,
            "ncm_config": "model = \"tok\"\nvocab_size = 32000\nd_model = 1024\nn_layers = 20\n",
            "ncm_quant": "q4-g64",
        }));
        let Header::Ncm(tok) = header(quantised) else {
            panic!("not read as NCM");
        };
        assert_eq!(
            (tok.arch, tok.quant.as_deref()),
            (NcmArch::Tok, Some("q4-g64"))
        );
    }

    #[test]
    fn ordinary_safetensors_files_are_not_ncm() {
        assert_eq!(
            header(fixture::file(&serde_json::json!({"format": "pt"}))),
            Header::NotNcm
        );
        assert_eq!(
            header(fixture::file(&serde_json::Value::Null)),
            Header::NotNcm
        );
        assert_eq!(header(vec![1, 2, 3]), Header::NotNcm);
        // A header too large to read without the marker is someone else's file.
        let mut big = (MAX_HEADER_BYTES + 10).to_le_bytes().to_vec();
        big.extend(vec![b' '; 4096]);
        assert_eq!(header(big), Header::NotNcm);
    }

    #[test]
    fn an_oversized_or_broken_marked_header_is_a_problem_not_a_panic() {
        let mut big = (u64::MAX).to_le_bytes().to_vec();
        big.extend(b"{\"__metadata__\":{\"ncm_format\":\"ncm-safetensors-v1\"");
        let Header::Problem(problem) = header(big) else {
            panic!("not a problem");
        };
        assert!(problem.contains("too large"), "{problem}");

        let mut short = 4096_u64.to_le_bytes().to_vec();
        short.extend(b"{\"__metadata__\":{\"ncm_format\":");
        assert!(matches!(header(short), Header::Problem(p) if p.contains("past the end")));

        let mut broken = 40_u64.to_le_bytes().to_vec();
        broken.extend(b"{\"ncm_format\": [[[[[[[[[[[[[[[[[[[[[[[[[");
        assert!(matches!(header(broken), Header::Problem(p) if p.contains("JSON")));
    }

    #[test]
    fn header_fields_are_limited_and_numbers_checked() {
        let with = |metadata: serde_json::Value| header(fixture::file(&metadata));
        let config =
            |text: &str| with(serde_json::json!({"ncm_format": NCM_FORMAT, "ncm_config": text}));
        for bad in [
            "model = \"ncm\"\nvocab_size = 0\nk = 4\nbb_dim = 1024\nbb_layers = 16",
            "model = \"ncm\"\nvocab_size = 32000\nk = 4\nbb_dim = 1024\nbb_layers = 99999999999",
            "model = \"ncm\"\nvocab_size = -5\nk = 4\nbb_dim = 1024\nbb_layers = 16",
            "model = \"ncm\"\nvocab_size = 32000\nk = 0\nbb_dim = 1024\nbb_layers = 16",
            "model = \"ncm\"\nvocab_size = 32000\nk = 4\nbb_dim = 1.5\nbb_layers = 16",
            "model = \"ncm\"\nvocab_size = 32000\nk = 4\nbb_layers = 16",
            "model = \"mamba\"",
            "not toml at all = = =",
        ] {
            assert!(matches!(config(bad), Header::Problem(_)), "{bad}");
        }
        let long = format!("model = \"ncm\"\n# {}", "x".repeat(MAX_CONFIG_BYTES));
        assert!(matches!(config(&long), Header::Problem(p) if p.contains("longer")));
        assert!(matches!(
            with(serde_json::json!({"ncm_format": "ncm-safetensors-v9", "ncm_config": fixture::NCM_CONFIG})),
            Header::Problem(p) if p.contains("not one this Imperium reads")
        ));
        assert!(matches!(
            with(serde_json::json!({"ncm_format": NCM_FORMAT})),
            Header::Problem(p) if p.contains("no ncm_config")
        ));
        assert!(matches!(
            with(serde_json::json!({
                "ncm_format": NCM_FORMAT,
                "ncm_config": fixture::NCM_CONFIG,
                "ncm_quant": "q".repeat(MAX_QUANT_CHARS + 1),
            })),
            Header::Problem(_)
        ));
    }

    #[test]
    fn cache_sizes_follow_the_published_values_and_never_overflow() {
        let Header::Ncm(ncm) = header(fixture::ncm()) else {
            panic!();
        };
        let tok = NcmShape {
            arch: NcmArch::Tok,
            ..ncm.clone()
        };
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        let at_1m = |shape: &NcmShape| cache_bytes(shape, NCM_MAX_CONTEXT, 2).unwrap() as f64 / GIB;
        // Published: about 1.01 GiB (NCM) and 5.06 GiB (token baseline) at 1M tokens in bf16;
        // the estimate may be above them, never more than 15 % or below.
        assert!((1.01..1.17).contains(&at_1m(&ncm)), "{}", at_1m(&ncm));
        assert!((5.06..5.82).contains(&at_1m(&tok)), "{}", at_1m(&tok));
        // Tens of MiB at 32k.
        let at_32k = cache_bytes(&ncm, 32_768, 2).unwrap();
        assert!(at_32k < 128 * 1024 * 1024, "{at_32k}");
        assert_eq!(cache_bytes(&ncm, u64::MAX, 2), None);
        assert_eq!(cache_bytes(&ncm, 1, u64::MAX), None);
    }

    #[test]
    fn a_missing_tokenizer_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let weights = dir.path().join("princeps-ncm-250m.safetensors");
        std::fs::write(&weights, fixture::ncm()).unwrap();
        assert!(missing_tokenizer(&weights).is_some());
        std::fs::write(dir.path().join("tokenizer.json"), "{}").unwrap();
        assert_eq!(missing_tokenizer(&weights), None);
    }
}
