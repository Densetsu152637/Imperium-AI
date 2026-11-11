//! Reads the metadata header of a GGUF file: enough to describe a model and plan its memory
//! without loading its weights. Large arrays (the tokenizer's vocabulary) are skipped, not read.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: &[u8; 4] = b"GGUF";
/// Longest metadata string kept; longer ones (chat templates) are skipped.
const MAX_STRING: u64 = 64 * 1024;

/// What the header says about a model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GgufInfo {
    pub version: u32,
    pub architecture: Option<String>,
    pub name: Option<String>,
    /// Transformer blocks.
    pub block_count: Option<u64>,
    pub embedding_length: Option<u64>,
    pub head_count: Option<u64>,
    /// Key/value heads; fewer than `head_count` with grouped-query attention.
    pub head_count_kv: Option<u64>,
    /// Per-head key size, when the model declares it.
    pub key_length: Option<u64>,
    pub value_length: Option<u64>,
    /// Longest context the model was trained for.
    pub context_length: Option<u64>,
    /// `general.file_type`: the dominant quantisation.
    pub file_type: Option<u32>,
    /// For split models: which part this is (0-based) and how many there are.
    pub split_index: Option<u32>,
    pub split_count: Option<u32>,
    /// What the model card fields of the header say (`general.*`), for the model's card.
    pub about: About,
}

/// The descriptive `general.*` fields of a GGUF header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct About {
    /// `general.basename`, such as `Qwen3-Coder`.
    pub basename: Option<String>,
    /// `general.finetune`, such as `Instruct`.
    pub finetune: Option<String>,
    /// `general.size_label`, such as `30B-A3B`.
    pub size_label: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    /// `general.tags`, such as `text-generation` or `code`.
    pub tags: Vec<String>,
    /// `general.languages`.
    pub languages: Vec<String>,
}

impl GgufInfo {
    /// Bytes of KV cache per token of context at the given cache element size, when the
    /// header has what it takes: layers × KV heads × (key + value head size) × element bytes.
    pub fn kv_bytes_per_token(&self, cache_element_bytes: f64) -> Option<u64> {
        self.kv_bytes_per_token_kv(cache_element_bytes, cache_element_bytes)
    }

    /// Like [`GgufInfo::kv_bytes_per_token`], with separate element sizes for the key and the
    /// value cache.
    pub fn kv_bytes_per_token_kv(&self, key_bytes: f64, value_bytes: f64) -> Option<u64> {
        let layers = self.block_count?;
        let heads = self.head_count?;
        let kv_heads = self.head_count_kv.unwrap_or(heads);
        let head_dim = self.embedding_length? / heads.max(1);
        let key = self.key_length.unwrap_or(head_dim);
        let value = self.value_length.unwrap_or(head_dim);
        let bytes =
            (layers * kv_heads) as f64 * (key as f64 * key_bytes + value as f64 * value_bytes);
        Some(bytes.ceil() as u64)
    }

    /// The quantisation's usual name, such as `Q4_K_M`.
    pub fn quantization(&self) -> Option<&'static str> {
        Some(match self.file_type? {
            0 => "F32",
            1 => "F16",
            2 => "Q4_0",
            3 => "Q4_1",
            7 => "Q8_0",
            8 => "Q5_0",
            9 => "Q5_1",
            10 => "Q2_K",
            11 => "Q3_K_S",
            12 => "Q3_K_M",
            13 => "Q3_K_L",
            14 => "Q4_K_S",
            15 => "Q4_K_M",
            16 => "Q5_K_S",
            17 => "Q5_K_M",
            18 => "Q6_K",
            19 => "IQ2_XXS",
            20 => "IQ2_XS",
            21 => "Q2_K_S",
            22 => "IQ3_XS",
            23 => "IQ3_XXS",
            24 => "IQ1_S",
            25 => "IQ4_NL",
            26 => "IQ3_S",
            27 => "IQ3_M",
            28 => "IQ2_S",
            29 => "IQ2_M",
            30 => "IQ4_XS",
            31 => "IQ1_M",
            32 => "BF16",
            36 => "TQ1_0",
            37 => "TQ2_0",
            38 => "MXFP4",
            _ => return None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GgufError {
    #[error("not a GGUF file")]
    NotGguf,
    #[error("unsupported GGUF version {0}")]
    Version(u32),
    #[error("malformed GGUF header: {0}")]
    Malformed(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Reads the header of the GGUF file at `path`.
pub fn read_info(path: &Path) -> Result<GgufInfo, GgufError> {
    read(&mut BufReader::new(File::open(path)?))
}

/// Reads a GGUF header from `reader`, which must be at the start of the file.
pub fn read(reader: &mut (impl Read + Seek)) -> Result<GgufInfo, GgufError> {
    let mut magic = [0; 4];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(GgufError::NotGguf);
    }
    let version = read_u32(reader)?;
    if !(2..=3).contains(&version) {
        return Err(GgufError::Version(version));
    }
    let _tensor_count = read_u64(reader)?;
    let kv_count = read_u64(reader)?;
    let mut info = GgufInfo {
        version,
        ..GgufInfo::default()
    };
    // Keys are prefixed by the architecture, which `general.architecture` names (first, by
    // convention, but not relied on): collect the numbers, then pick the architecture's.
    let mut numbers = Vec::new();
    for _ in 0..kv_count {
        let key = read_string(reader)?.ok_or(GgufError::Malformed("key too long"))?;
        let kind = read_u32(reader)?;
        let texts = matches!(key.as_str(), "general.tags" | "general.languages");
        match read_value(reader, kind, texts)? {
            Value::Text(text) => {
                let about = &mut info.about;
                match key.as_str() {
                    "general.architecture" => info.architecture = Some(text),
                    "general.name" => info.name = Some(text),
                    "general.basename" => about.basename = Some(text),
                    "general.finetune" => about.finetune = Some(text),
                    "general.size_label" => about.size_label = Some(text),
                    "general.description" => about.description = Some(text),
                    "general.license" => about.license = Some(text),
                    _ => {}
                }
            }
            Value::Texts(list) => match key.as_str() {
                "general.tags" => info.about.tags = list,
                "general.languages" => info.about.languages = list,
                _ => {}
            },
            Value::Number(number) => numbers.push((key, number)),
            Value::Skipped => {}
        }
    }
    let arch = info.architecture.clone().unwrap_or_default();
    for (key, number) in numbers {
        let field = match key
            .strip_prefix(arch.as_str())
            .and_then(|k| k.strip_prefix('.'))
        {
            Some(field) => field,
            None => {
                match key.as_str() {
                    "general.file_type" => info.file_type = u32::try_from(number).ok(),
                    "split.no" => info.split_index = u32::try_from(number).ok(),
                    "split.count" => info.split_count = u32::try_from(number).ok(),
                    _ => {}
                }
                continue;
            }
        };
        let slot = match field {
            "block_count" => &mut info.block_count,
            "embedding_length" => &mut info.embedding_length,
            "attention.head_count" => &mut info.head_count,
            "attention.head_count_kv" => &mut info.head_count_kv,
            "attention.key_length" => &mut info.key_length,
            "attention.value_length" => &mut info.value_length,
            "context_length" => &mut info.context_length,
            _ => continue,
        };
        *slot = Some(number);
    }
    Ok(info)
}

enum Value {
    Text(String),
    /// A short array of strings, kept only where asked for.
    Texts(Vec<String>),
    Number(u64),
    Skipped,
}

/// Most strings of an array kept as [`Value::Texts`]; longer arrays (the vocabulary) are skipped.
const MAX_TEXTS: u64 = 64;

// GGUF value types.
const UINT8: u32 = 0;
const INT8: u32 = 1;
const UINT16: u32 = 2;
const INT16: u32 = 3;
const UINT32: u32 = 4;
const INT32: u32 = 5;
const FLOAT32: u32 = 6;
const BOOL: u32 = 7;
const STRING: u32 = 8;
const ARRAY: u32 = 9;
const UINT64: u32 = 10;
const INT64: u32 = 11;
const FLOAT64: u32 = 12;

/// Reads one value; with `texts`, a short array of strings is kept as [`Value::Texts`].
fn read_value(reader: &mut (impl Read + Seek), kind: u32, texts: bool) -> Result<Value, GgufError> {
    Ok(match kind {
        UINT8 | INT8 | BOOL => Value::Number(u64::from(read_array::<1>(reader)?[0])),
        UINT16 | INT16 => Value::Number(u64::from(u16::from_le_bytes(read_array(reader)?))),
        UINT32 | INT32 => Value::Number(u64::from(read_u32(reader)?)),
        UINT64 | INT64 => Value::Number(read_u64(reader)?),
        FLOAT32 => {
            skip(reader, 4)?;
            Value::Skipped
        }
        FLOAT64 => {
            skip(reader, 8)?;
            Value::Skipped
        }
        STRING => read_string(reader)?.map_or(Value::Skipped, Value::Text),
        ARRAY => {
            let element = read_u32(reader)?;
            let count = read_u64(reader)?;
            if texts && element == STRING && count <= MAX_TEXTS {
                let mut list = Vec::new();
                for _ in 0..count {
                    if let Some(text) = read_string(reader)? {
                        list.push(text);
                    }
                }
                return Ok(Value::Texts(list));
            }
            match fixed_size(element) {
                Some(size) => skip(
                    reader,
                    count
                        .checked_mul(size)
                        .ok_or(GgufError::Malformed("array too large"))?,
                )?,
                None => {
                    for _ in 0..count {
                        read_value(reader, element, false)?;
                    }
                }
            }
            Value::Skipped
        }
        _ => return Err(GgufError::Malformed("unknown value type")),
    })
}

fn fixed_size(kind: u32) -> Option<u64> {
    match kind {
        UINT8 | INT8 | BOOL => Some(1),
        UINT16 | INT16 => Some(2),
        UINT32 | INT32 | FLOAT32 => Some(4),
        UINT64 | INT64 | FLOAT64 => Some(8),
        _ => None,
    }
}

/// A string, or `None` (after skipping it) when it is longer than [`MAX_STRING`].
fn read_string(reader: &mut (impl Read + Seek)) -> Result<Option<String>, GgufError> {
    let len = read_u64(reader)?;
    if len > MAX_STRING {
        skip(reader, len)?;
        return Ok(None);
    }
    let mut bytes = vec![0; usize::try_from(len).map_err(|_| GgufError::Malformed("length"))?];
    reader.read_exact(&mut bytes)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn skip(reader: &mut impl Seek, bytes: u64) -> Result<(), GgufError> {
    let offset = i64::try_from(bytes).map_err(|_| GgufError::Malformed("length"))?;
    reader.seek(SeekFrom::Current(offset))?;
    Ok(())
}

fn read_array<const N: usize>(reader: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    read_array(reader).map(u32::from_le_bytes)
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    read_array(reader).map(u64::from_le_bytes)
}

/// Writes GGUF headers for tests.
#[cfg(test)]
pub(crate) mod fixture {
    pub enum V<'a> {
        Str(&'a str),
        U32(u32),
        U64(u64),
        F32(f32),
        StrArray(&'a [&'a str]),
        I32Array(&'a [i32]),
    }

    pub fn header(pairs: &[(&str, V<'_>)]) -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend(0u64.to_le_bytes());
        out.extend((pairs.len() as u64).to_le_bytes());
        let string = |out: &mut Vec<u8>, text: &str| {
            out.extend((text.len() as u64).to_le_bytes());
            out.extend(text.as_bytes());
        };
        for (key, value) in pairs {
            string(&mut out, key);
            match value {
                V::Str(text) => {
                    out.extend(8u32.to_le_bytes());
                    string(&mut out, text);
                }
                V::U32(n) => {
                    out.extend(4u32.to_le_bytes());
                    out.extend(n.to_le_bytes());
                }
                V::U64(n) => {
                    out.extend(10u32.to_le_bytes());
                    out.extend(n.to_le_bytes());
                }
                V::F32(n) => {
                    out.extend(6u32.to_le_bytes());
                    out.extend(n.to_le_bytes());
                }
                V::StrArray(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(8u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for item in *items {
                        string(&mut out, item);
                    }
                }
                V::I32Array(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(5u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for item in *items {
                        out.extend(item.to_le_bytes());
                    }
                }
            }
        }
        out
    }

    /// A small Llama-shaped model header: 32 layers, 32 heads, 8 KV heads, 4096 wide.
    pub fn llama(name: &str) -> Vec<u8> {
        header(&[
            ("general.architecture", V::Str("llama")),
            ("general.name", V::Str(name)),
            ("general.file_type", V::U32(15)),
            ("llama.block_count", V::U32(32)),
            ("llama.context_length", V::U32(131_072)),
            ("llama.embedding_length", V::U32(4096)),
            ("llama.attention.head_count", V::U32(32)),
            ("llama.attention.head_count_kv", V::U32(8)),
            ("llama.rope.freq_base", V::F32(500_000.0)),
            (
                "tokenizer.ggml.tokens",
                V::StrArray(&["<s>", "</s>", "hello"]),
            ),
            ("tokenizer.ggml.token_type", V::I32Array(&[3, 3, 1])),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{V, header, llama};
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_architecture_fields_and_skips_arrays() {
        let info = read(&mut Cursor::new(llama("Test 8B"))).unwrap();
        assert_eq!(info.architecture.as_deref(), Some("llama"));
        assert_eq!(info.name.as_deref(), Some("Test 8B"));
        assert_eq!(info.block_count, Some(32));
        assert_eq!(info.head_count_kv, Some(8));
        assert_eq!(info.context_length, Some(131_072));
        assert_eq!(info.quantization(), Some("Q4_K_M"));
        // 32 layers × 8 KV heads × (128 + 128) × 2 bytes (f16) = 128 KiB per token.
        assert_eq!(info.kv_bytes_per_token(2.0), Some(131_072));
    }

    #[test]
    fn the_model_card_fields_are_read() {
        let bytes = header(&[
            ("general.architecture", V::Str("qwen3")),
            ("general.basename", V::Str("Qwen3-Coder")),
            ("general.finetune", V::Str("Instruct")),
            ("general.size_label", V::Str("30B-A3B")),
            ("general.license", V::Str("apache-2.0")),
            ("general.description", V::Str("A coding model.")),
            ("general.tags", V::StrArray(&["code", "text-generation"])),
            ("general.languages", V::StrArray(&["en"])),
            ("tokenizer.ggml.tokens", V::StrArray(&["a", "b"])),
            ("qwen3.block_count", V::U32(48)),
        ]);
        let info = read(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(info.about.basename.as_deref(), Some("Qwen3-Coder"));
        assert_eq!(info.about.finetune.as_deref(), Some("Instruct"));
        assert_eq!(info.about.size_label.as_deref(), Some("30B-A3B"));
        assert_eq!(info.about.license.as_deref(), Some("apache-2.0"));
        assert_eq!(info.about.description.as_deref(), Some("A coding model."));
        assert_eq!(info.about.tags, ["code", "text-generation"]);
        assert_eq!(info.about.languages, ["en"]);
        // Arrays after the kept ones are still skipped correctly.
        assert_eq!(info.block_count, Some(48));
    }

    #[test]
    fn explicit_head_sizes_and_split_parts_are_read() {
        let bytes = header(&[
            ("general.architecture", V::Str("qwen3")),
            ("qwen3.block_count", V::U64(48)),
            ("qwen3.embedding_length", V::U64(2048)),
            ("qwen3.attention.head_count", V::U32(32)),
            ("qwen3.attention.head_count_kv", V::U32(4)),
            ("qwen3.attention.key_length", V::U32(128)),
            ("qwen3.attention.value_length", V::U32(128)),
            ("split.no", V::U32(1)),
            ("split.count", V::U32(3)),
        ]);
        let info = read(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(info.kv_bytes_per_token(1.0), Some(48 * 4 * 256));
        assert_eq!((info.split_index, info.split_count), (Some(1), Some(3)));
    }

    #[test]
    fn other_files_are_rejected() {
        assert!(matches!(
            read(&mut Cursor::new(b"PK\x03\x04rest".to_vec())),
            Err(GgufError::NotGguf)
        ));
        let mut truncated = llama("x");
        truncated.truncate(40);
        assert!(matches!(
            read(&mut Cursor::new(truncated)),
            Err(GgufError::Io(_))
        ));
    }
}
