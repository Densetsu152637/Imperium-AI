//! Hugging Face Hub downloads: listing a repository's files, choosing the ones a runtime needs
//! (a GGUF quantisation for llama.cpp, or a safetensors folder for vLLM), and downloading them
//! with resume and SHA256 checks into `<dir>/<owner>/<repo>/`, where the scanner finds them.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use imperium_config::SecretStore;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, LINK, RANGE};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::scan::split_part;

/// The public Hub; `HF_ENDPOINT` overrides it, as it does for `huggingface_hub`.
pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";
/// The quantisation chosen when a request names neither a quantisation nor a file.
pub const DEFAULT_QUANT: &str = "Q4_K_M";
/// The keychain secret read for a token when `HF_TOKEN` is not set.
pub const TOKEN_SECRET: &str = "huggingface";

/// A file in a repository, as the tree listing reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFile {
    /// Relative to the repository root, with `/` separators.
    pub path: String,
    pub size: u64,
    /// The LFS object id, which is the content's SHA256; small files stored in Git have none.
    pub sha256: Option<String>,
}

/// What a set of files is served by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weights {
    /// GGUF files, served by llama.cpp.
    Gguf,
    /// A Hugging Face folder of safetensors, served by vLLM.
    Safetensors,
}

impl Weights {
    /// The `[[models.catalog]]` endpoint that serves these weights.
    pub fn runtime(self) -> &'static str {
        match self {
            Weights::Gguf => "llamacpp",
            Weights::Safetensors => "vllm",
        }
    }
}

/// One downloadable model in a repository: a GGUF quantisation (every part of a split set) or
/// the safetensors folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    /// The quantisation, such as `Q4_K_M`, or `safetensors`.
    pub name: String,
    pub weights: Weights,
    pub files: Vec<RepoFile>,
}

impl Variant {
    pub fn size_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// The id the scanner will give the downloaded model: the GGUF file name without `.gguf`
    /// and the split suffix, or the repository's name for a safetensors folder.
    pub fn model_id(&self, repo: &str) -> String {
        match self.weights {
            Weights::Safetensors => repo.rsplit('/').next().unwrap_or(repo).to_string(),
            Weights::Gguf => {
                let first = self
                    .files
                    .iter()
                    .find(|file| !is_projector(&file.path))
                    .or(self.files.first());
                let stem = first.map_or("", |file| gguf_stem(&file.path));
                split_part(stem)
                    .map_or(stem, |(base, _, _)| base)
                    .to_string()
            }
        }
    }
}

/// What to download from a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choice {
    /// A GGUF quantisation, matched case-insensitively; [`DEFAULT_QUANT`] when neither this
    /// nor `file` is set and the repository has more than one.
    pub quant: Option<String>,
    /// A file by path or name; every part when it belongs to a split set.
    pub file: Option<String>,
    /// Also download the multimodal projector (`mmproj`) of a GGUF repository.
    pub mmproj: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HfError {
    #[error("{0:?} is not a Hugging Face repository id; use owner/name")]
    InvalidRepo(String),
    #[error(
        "{repo} needs a Hugging Face token (it is gated or private): set HF_TOKEN, or store one with `imperium secret set {TOKEN_SECRET}`"
    )]
    TokenNeeded { repo: String },
    #[error("the Hugging Face token was refused for {repo}; check that it is valid")]
    TokenRefused { repo: String },
    #[error(
        "access to {repo} is restricted: accept its terms at {url} with the account the token belongs to"
    )]
    Forbidden { repo: String, url: String },
    #[error("{repo} at revision {revision} was not found{hint}")]
    NotFound {
        repo: String,
        revision: String,
        hint: &'static str,
    },
    #[error("{url}: {message}")]
    Http { url: String, message: String },
    #[error("the file listing of {repo} could not be read: {message}")]
    Listing { repo: String, message: String },
    #[error("{repo} lists an unsafe file path {path:?}")]
    UnsafePath { repo: String, path: String },
    #[error("{repo} has no GGUF or safetensors weights")]
    NoWeights { repo: String },
    #[error("no GGUF file in {repo} has quantisation {quant}; it has {}", .available.join(", "))]
    NoQuant {
        repo: String,
        quant: String,
        available: Vec<String>,
    },
    #[error("quantisation {quant} matches several file sets in {repo}; pick one with --file: {}", .candidates.join(", "))]
    Ambiguous {
        repo: String,
        quant: String,
        candidates: Vec<String>,
    },
    #[error("{repo} has no file {file:?}")]
    NoFile { repo: String, file: String },
    #[error("{file} is {actual} bytes, expected {expected}")]
    Size {
        file: String,
        expected: u64,
        actual: u64,
    },
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

fn io(context: &Path) -> impl FnOnce(std::io::Error) -> HfError + '_ {
    move |error| HfError::Io(context.display().to_string(), error)
}

/// Progress through a download, reported after every chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress<'a> {
    /// The file being downloaded, as the repository names it.
    pub file: &'a str,
    pub file_done: u64,
    pub file_total: u64,
    /// Across every file of the download.
    pub done: u64,
    pub total: u64,
}

/// A Hub to list and download from. The token is sent as `Authorization: Bearer` and is never
/// logged or shown by `Debug`.
#[derive(Clone)]
pub struct Hub {
    http: reqwest::Client,
    endpoint: String,
    token: Option<String>,
}

impl fmt::Debug for Hub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hub")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

impl Hub {
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Self {
        // A stalled connection fails instead of hanging; the `.part` file lets it resume.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Hub {
            http,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token: token.filter(|token| !token.is_empty()),
        }
    }

    /// The Hub at `HF_ENDPOINT` (or [`DEFAULT_ENDPOINT`]), with a token from `HF_TOKEN` or, when
    /// that is unset or empty, the secret [`TOKEN_SECRET`] in `secrets`.
    pub fn from_env(secrets: &dyn SecretStore) -> Self {
        let endpoint = std::env::var("HF_ENDPOINT")
            .ok()
            .filter(|endpoint| !endpoint.is_empty())
            .unwrap_or_else(|| DEFAULT_ENDPOINT.into());
        let token = std::env::var("HF_TOKEN")
            .ok()
            .filter(|token| !token.is_empty())
            .or_else(|| {
                let name = TOKEN_SECRET.parse().ok()?;
                secrets.load(&name).ok().flatten()
            });
        Hub::new(endpoint, token)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Every file in `repo` at `revision`, following the listing's pages.
    pub async fn files(&self, repo: &str, revision: &str) -> Result<Vec<RepoFile>, HfError> {
        check_repo(repo)?;
        let mut url = format!(
            "{}/api/models/{repo}/tree/{}?recursive=true",
            self.endpoint,
            encode(revision)
        );
        let mut files = Vec::new();
        loop {
            let response = self.get(&url, None, repo, revision).await?;
            let next = next_page(response.headers(), &self.endpoint);
            let listing: serde_json::Value =
                response.json().await.map_err(|error| HfError::Listing {
                    repo: repo.into(),
                    message: error.to_string(),
                })?;
            let entries = listing.as_array().ok_or_else(|| HfError::Listing {
                repo: repo.into(),
                message: "expected a list of files".into(),
            })?;
            for entry in entries {
                if entry["type"] != "file" {
                    continue;
                }
                let path = entry["path"].as_str().ok_or_else(|| HfError::Listing {
                    repo: repo.into(),
                    message: "a file has no path".into(),
                })?;
                check_path(repo, path)?;
                let sha256 = entry["lfs"]["oid"]
                    .as_str()
                    .filter(|oid| oid.len() == 64 && oid.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_ascii_lowercase);
                files.push(RepoFile {
                    path: path.to_string(),
                    size: entry["size"].as_u64().unwrap_or(0),
                    sha256,
                });
            }
            match next {
                Some(next) => url = next,
                None => break,
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// Downloads `files` from `repo` into `dir`, keeping each file's path in the repository.
    /// A file already present with the expected size is kept; a `<file>.part` left by an
    /// interrupted download is resumed. Returns the downloaded paths.
    pub async fn download(
        &self,
        repo: &str,
        revision: &str,
        files: &[RepoFile],
        dir: &Path,
        mut progress: impl FnMut(Progress<'_>) + Send,
    ) -> Result<Vec<PathBuf>, HfError> {
        check_repo(repo)?;
        let total = files.iter().map(|file| file.size).sum();
        let mut done = 0;
        let mut paths = Vec::new();
        for file in files {
            check_path(repo, &file.path)?;
            let target = dir.join(&file.path);
            let present = tokio::fs::metadata(&target)
                .await
                .is_ok_and(|meta| meta.is_file() && meta.len() == file.size);
            if !present {
                let url = format!(
                    "{}/{repo}/resolve/{}/{}",
                    self.endpoint,
                    encode(revision),
                    file.path
                        .split('/')
                        .map(encode)
                        .collect::<Vec<_>>()
                        .join("/")
                );
                let before = done;
                self.fetch(repo, revision, &url, file, &target, &mut |file_done| {
                    progress(Progress {
                        file: &file.path,
                        file_done,
                        file_total: file.size,
                        done: before + file_done,
                        total,
                    });
                })
                .await?;
            }
            done += file.size;
            progress(Progress {
                file: &file.path,
                file_done: file.size,
                file_total: file.size,
                done,
                total,
            });
            paths.push(target);
        }
        Ok(paths)
    }

    /// Downloads one file to `<target>.part`, resuming it when present, then checks its size
    /// and checksum and moves it into place.
    async fn fetch(
        &self,
        repo: &str,
        revision: &str,
        url: &str,
        file: &RepoFile,
        target: &Path,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), HfError> {
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(io(parent))?;
        }
        let mut part_name = target.file_name().unwrap_or_default().to_os_string();
        part_name.push(".part");
        let part = target.with_file_name(part_name);
        let mut have = tokio::fs::metadata(&part)
            .await
            .map_or(0, |meta| meta.len());
        if have > file.size {
            have = 0;
        }
        let mut hasher = Sha256::new();
        if have > 0 && file.sha256.is_some() {
            hash_prefix(&part, have, &mut hasher).await?;
        }
        if have < file.size || file.size == 0 {
            let range = (have > 0).then(|| format!("bytes={have}-"));
            let response = self.get(url, range.as_deref(), repo, revision).await?;
            let resumed = response.status() == StatusCode::PARTIAL_CONTENT;
            if !resumed {
                // The server sent the whole file (or no range was asked for): start over.
                have = 0;
                hasher = Sha256::new();
            }
            let mut out = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(resumed)
                .truncate(!resumed)
                .open(&part)
                .await
                .map_err(io(&part))?;
            let mut body = response.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|error| HfError::Http {
                    url: url.to_string(),
                    message: error.to_string(),
                })?;
                hasher.update(&chunk);
                out.write_all(&chunk).await.map_err(io(&part))?;
                have += chunk.len() as u64;
                progress(have);
            }
            out.flush().await.map_err(io(&part))?;
        }
        if have != file.size {
            // A short body can be resumed; a long one cannot be right.
            if have > file.size {
                let _ = tokio::fs::remove_file(&part).await;
            }
            return Err(HfError::Size {
                file: file.path.clone(),
                expected: file.size,
                actual: have,
            });
        }
        if let Some(expected) = &file.sha256 {
            let actual = hex(&hasher.finalize());
            if &actual != expected {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(HfError::Checksum {
                    file: file.path.clone(),
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        tokio::fs::rename(&part, target).await.map_err(io(target))
    }

    /// Sends a GET with the token, turning refusals into errors that say what to do.
    async fn get(
        &self,
        url: &str,
        range: Option<&str>,
        repo: &str,
        revision: &str,
    ) -> Result<reqwest::Response, HfError> {
        let mut request = self.http.get(url);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(range) = range {
            request = request.header(RANGE, range);
        }
        let response = request.send().await.map_err(|error| HfError::Http {
            url: url.to_string(),
            message: error.to_string(),
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let code = response
            .headers()
            .get("x-error-code")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let not_found = |hint| HfError::NotFound {
            repo: repo.into(),
            revision: revision.into(),
            hint,
        };
        Err(match status {
            // The Hub answers 401 for private repositories and for ones that do not exist.
            StatusCode::UNAUTHORIZED if self.token.is_none() && code == "RepoNotFound" => {
                not_found(" (if it is private, set HF_TOKEN or the huggingface secret)")
            }
            StatusCode::UNAUTHORIZED if self.token.is_none() => {
                HfError::TokenNeeded { repo: repo.into() }
            }
            StatusCode::UNAUTHORIZED => HfError::TokenRefused { repo: repo.into() },
            StatusCode::FORBIDDEN => HfError::Forbidden {
                repo: repo.into(),
                url: format!("{}/{repo}", self.endpoint),
            },
            StatusCode::NOT_FOUND => not_found(""),
            status => HfError::Http {
                url: url.to_string(),
                message: format!("HTTP {status}"),
            },
        })
    }
}

/// Reads the first `len` bytes of an interrupted download into `hasher`.
async fn hash_prefix(part: &Path, len: u64, hasher: &mut Sha256) -> Result<(), HfError> {
    let file = tokio::fs::File::open(part).await.map_err(io(part))?;
    let mut reader = file.take(len);
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = reader.read(&mut buffer).await.map_err(io(part))?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

/// The `rel="next"` page of a paginated listing, when it is on the same Hub (so the token is
/// never sent elsewhere).
fn next_page(headers: &reqwest::header::HeaderMap, endpoint: &str) -> Option<String> {
    let link = headers.get(LINK)?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        let next = params
            .split(';')
            .any(|param| param.trim().replace(' ', "") == "rel=\"next\"");
        (next && target.starts_with(&format!("{endpoint}/"))).then(|| target.to_string())
    })
}

/// Repository ids are `owner/name`, each of letters, digits, `-`, `_`, and `.`.
fn check_repo(repo: &str) -> Result<(), HfError> {
    let part = |text: &str| {
        !text.is_empty()
            && text != "."
            && text != ".."
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if part(owner) && part(name) => Ok(()),
        _ => Err(HfError::InvalidRepo(repo.into())),
    }
}

/// Paths come from the server, so each must stay inside the download folder.
fn check_path(repo: &str, path: &str) -> Result<(), HfError> {
    let safe = !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if safe {
        Ok(())
    } else {
        Err(HfError::UnsafePath {
            repo: repo.into(),
            path: path.into(),
        })
    }
}

/// Percent-encodes everything but unreserved characters, so a revision such as `refs/pr/1`
/// stays one path segment.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_gguf(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".gguf")
}

fn gguf_stem(path: &str) -> &str {
    let name = file_name(path);
    &name[..name.len().saturating_sub(".gguf".len())]
}

fn is_projector(path: &str) -> bool {
    file_name(path).to_ascii_lowercase().contains("mmproj")
}

/// Whether a name token is a llama.cpp quantisation, such as `Q4_K_M`, `IQ4_XS`, or `BF16`.
fn is_quant(token: &str) -> bool {
    let token = token.to_ascii_uppercase();
    let numbered = |prefix: &str| {
        token
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    numbered("Q")
        || numbered("IQ")
        || numbered("TQ")
        || token.starts_with("MXFP4")
        || matches!(
            token.as_str(),
            "F16" | "BF16" | "F32" | "FP16" | "FP32" | "FP8"
        )
}

/// The quantisation a GGUF file's name (or, failing that, its folder) shows; the file name
/// itself when neither does.
fn quant_label(path: &str) -> String {
    let stem = gguf_stem(path);
    let stem = split_part(stem).map_or(stem, |(base, _, _)| base);
    let in_name = stem.split(['-', '.']).rev().find(|token| is_quant(token));
    let in_folder = || {
        path.rsplit('/')
            .skip(1)
            .flat_map(|dir| dir.split(['-', '.']).rev())
            .find(|token| is_quant(token))
    };
    in_name.or_else(in_folder).unwrap_or(stem).to_string()
}

/// Files a vLLM folder needs, besides the safetensors themselves.
fn is_vllm_support_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".json")
        || lower.ends_with(".jinja")
        || lower.ends_with(".tiktoken")
        || matches!(
            lower.as_str(),
            "tokenizer.model" | "merges.txt" | "vocab.txt" | "spiece.model"
        )
}

/// The models a repository offers: each GGUF quantisation (split sets as one, projectors left
/// out) in name order, then the safetensors folder when there is one.
pub fn variants(files: &[RepoFile]) -> Vec<Variant> {
    let mut sets: Vec<(String, Variant)> = Vec::new();
    for file in files {
        if !is_gguf(&file.path) || is_projector(&file.path) {
            continue;
        }
        let stem = gguf_stem(&file.path);
        let base = split_part(stem).map_or(stem, |(base, _, _)| base);
        let dir = file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let key = format!("{dir}/{base}");
        match sets.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, variant)) => variant.files.push(file.clone()),
            None => sets.push((
                key,
                Variant {
                    name: quant_label(&file.path),
                    weights: Weights::Gguf,
                    files: vec![file.clone()],
                },
            )),
        }
    }
    let mut variants: Vec<Variant> = sets.into_iter().map(|(_, variant)| variant).collect();
    variants.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.files[0].path.cmp(&b.files[0].path))
    });

    // Only top-level files: subfolders hold things such as `original/` checkpoints or ONNX.
    let top: Vec<&RepoFile> = files.iter().filter(|f| !f.path.contains('/')).collect();
    let has_config = top.iter().any(|file| file.path == "config.json");
    if has_config && top.iter().any(|file| file.path.ends_with(".safetensors")) {
        variants.push(Variant {
            name: "safetensors".into(),
            weights: Weights::Safetensors,
            files: top
                .into_iter()
                .filter(|file| {
                    file.path.ends_with(".safetensors") || is_vllm_support_file(&file.path)
                })
                .cloned()
                .collect(),
        });
    }
    variants
}

/// Multimodal projectors in a GGUF repository.
pub fn projectors(files: &[RepoFile]) -> Vec<&RepoFile> {
    files
        .iter()
        .filter(|file| is_gguf(&file.path) && is_projector(&file.path))
        .collect()
}

/// The variant a request with no quantisation or file gets: the only one, else
/// [`DEFAULT_QUANT`], else the safetensors folder.
pub fn default_variant(variants: &[Variant]) -> Option<&Variant> {
    if let [only] = variants {
        return Some(only);
    }
    let named = |name: &str| {
        variants
            .iter()
            .find(|variant| variant.name.eq_ignore_ascii_case(name))
    };
    named(DEFAULT_QUANT).or_else(|| named("safetensors"))
}

/// Chooses what to download from `repo`'s `files`. A projector is added to GGUF downloads with
/// `mmproj` (the F16 one when there are several).
pub fn choose(repo: &str, files: &[RepoFile], choice: &Choice) -> Result<Variant, HfError> {
    let variants = variants(files);
    let mut chosen = if let Some(wanted) = &choice.file {
        let matches = |file: &RepoFile| file.path == *wanted || file_name(&file.path) == wanted;
        match variants
            .iter()
            .find(|variant| variant.files.iter().any(matches))
        {
            // A part of a GGUF set brings the whole set; a safetensors file brings just itself.
            Some(variant) if variant.weights == Weights::Gguf => variant.clone(),
            _ => {
                let file =
                    files
                        .iter()
                        .find(|file| matches(file))
                        .ok_or_else(|| HfError::NoFile {
                            repo: repo.into(),
                            file: wanted.clone(),
                        })?;
                Variant {
                    name: file_name(&file.path).to_string(),
                    weights: if is_gguf(&file.path) {
                        Weights::Gguf
                    } else {
                        Weights::Safetensors
                    },
                    files: vec![file.clone()],
                }
            }
        }
    } else {
        let quant = match &choice.quant {
            Some(quant) => quant.as_str(),
            None => match default_variant(&variants) {
                Some(variant) => variant.name.as_str(),
                None if variants.is_empty() => {
                    return Err(HfError::NoWeights { repo: repo.into() });
                }
                None => DEFAULT_QUANT,
            },
        };
        let found: Vec<&Variant> = variants
            .iter()
            .filter(|variant| variant.name.eq_ignore_ascii_case(quant))
            .collect();
        match found.as_slice() {
            [variant] => (*variant).clone(),
            [] if variants.is_empty() => return Err(HfError::NoWeights { repo: repo.into() }),
            [] => {
                return Err(HfError::NoQuant {
                    repo: repo.into(),
                    quant: quant.into(),
                    available: variants
                        .iter()
                        .map(|variant| variant.name.clone())
                        .collect(),
                });
            }
            several => {
                return Err(HfError::Ambiguous {
                    repo: repo.into(),
                    quant: quant.into(),
                    candidates: several
                        .iter()
                        .map(|variant| variant.files[0].path.clone())
                        .collect(),
                });
            }
        }
    };
    if choice.mmproj && chosen.weights == Weights::Gguf {
        let projectors = projectors(files);
        let projector = projectors
            .iter()
            .find(|file| file.path.to_ascii_uppercase().contains("F16"))
            .or(projectors.first());
        if let Some(projector) = projector
            && !chosen.files.contains(projector)
        {
            chosen.files.push((*projector).clone());
        }
    }
    Ok(chosen)
}

#[cfg(test)]
pub(crate) mod fake {
    //! A fake Hub serving a tree listing and file downloads, with `Range` support and an
    //! optional required token.

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path as UrlPath, State};
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use sha2::{Digest, Sha256};

    #[derive(Debug, Default)]
    pub struct Repo {
        /// Path to contents.
        pub files: BTreeMap<String, Vec<u8>>,
        /// Paths whose listed checksum is wrong, to test the check.
        pub bad_checksums: Vec<String>,
        /// When set, requests without `Bearer <token>` get 401.
        pub token: Option<String>,
        /// Paths whose downloads stop after this many bytes.
        pub truncate_at: BTreeMap<String, usize>,
    }

    #[derive(Debug, Default)]
    pub struct Shared {
        pub repo: Mutex<Repo>,
        /// Range headers seen, by path.
        pub ranges: Mutex<Vec<(String, Option<String>)>>,
    }

    fn authorised(shared: &Shared, headers: &HeaderMap) -> bool {
        let repo = shared.repo.lock().unwrap();
        repo.token.as_ref().is_none_or(|token| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                == Some(format!("Bearer {token}").as_str())
        })
    }

    async fn tree(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
        if !authorised(&shared, &headers) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let repo = shared.repo.lock().unwrap();
        let entries: Vec<serde_json::Value> = repo
            .files
            .iter()
            .map(|(path, body)| {
                let oid = if repo.bad_checksums.contains(path) {
                    "0".repeat(64)
                } else {
                    super::hex(&Sha256::digest(body))
                };
                serde_json::json!({
                    "type": "file", "path": path, "size": body.len(), "oid": "git",
                    "lfs": {"oid": oid, "size": body.len(), "pointerSize": 130}
                })
            })
            .chain([serde_json::json!({"type": "directory", "path": "sub", "size": 0})])
            .collect();
        axum::Json(entries).into_response()
    }

    async fn resolve(
        State(shared): State<Arc<Shared>>,
        UrlPath((_owner, _name, _revision, path)): UrlPath<(String, String, String, String)>,
        headers: HeaderMap,
    ) -> Response {
        if !authorised(&shared, &headers) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let range = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        shared
            .ranges
            .lock()
            .unwrap()
            .push((path.clone(), range.clone()));
        let repo = shared.repo.lock().unwrap();
        let Some(body) = repo.files.get(&path) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut body = body.clone();
        let start = range
            .as_deref()
            .and_then(|range| range.strip_prefix("bytes="))
            .and_then(|range| range.strip_suffix('-'))
            .and_then(|start| start.parse::<usize>().ok());
        if let Some(cut) = repo.truncate_at.get(&path) {
            body.truncate(*cut);
        }
        match start {
            Some(start) if start <= body.len() => {
                (StatusCode::PARTIAL_CONTENT, body[start..].to_vec()).into_response()
            }
            _ => body.into_response(),
        }
    }

    /// Serves `repo` at a local address; returns the endpoint and the shared state.
    pub async fn serve(repo: Repo) -> (String, Arc<Shared>) {
        let shared = Arc::new(Shared {
            repo: Mutex::new(repo),
            ranges: Mutex::default(),
        });
        let app = Router::new()
            .route("/api/models/{owner}/{name}/tree/{revision}", get(tree))
            .route("/{owner}/{name}/resolve/{revision}/{*path}", get(resolve))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (url, shared)
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Repo, serve};
    use super::*;

    fn file(path: &str, size: u64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size,
            sha256: None,
        }
    }

    fn repo(files: &[(&str, &[u8])]) -> Repo {
        Repo {
            files: files
                .iter()
                .map(|(path, body)| (path.to_string(), body.to_vec()))
                .collect(),
            ..Repo::default()
        }
    }

    #[test]
    fn quantisations_are_read_from_names_and_folders() {
        assert_eq!(quant_label("Qwen3-8B-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_label("phi-2.q8_0.gguf"), "q8_0");
        assert_eq!(quant_label("Llama-3.2-1B-UD-IQ2_XXS.gguf"), "IQ2_XXS");
        assert_eq!(quant_label("big-BF16-00001-of-00002.gguf"), "BF16");
        assert_eq!(quant_label("Q6_K/model-00001-of-00003.gguf"), "Q6_K");
        assert_eq!(quant_label("gpt-oss-20b-mxfp4.gguf"), "mxfp4");
        assert_eq!(quant_label("plain.gguf"), "plain");
    }

    #[test]
    fn gguf_repos_offer_each_quant_once_with_split_sets_together() {
        let files = [
            file("README.md", 10),
            file("Model-Q4_K_M.gguf", 400),
            file("Model-Q8_0-00001-of-00002.gguf", 500),
            file("Model-Q8_0-00002-of-00002.gguf", 300),
            file("mmproj-Model-F16.gguf", 50),
            file("mmproj-Model-Q8_0.gguf", 30),
        ];
        let variants = variants(&files);
        let names: Vec<_> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Q4_K_M", "Q8_0"]);
        assert_eq!(variants[1].size_bytes(), 800);
        assert_eq!(variants[1].model_id("o/r"), "Model-Q8_0");
        assert_eq!(projectors(&files).len(), 2);

        // No choice means Q4_K_M; quants match regardless of case; projectors only on request.
        let chosen = choose("o/r", &files, &Choice::default()).unwrap();
        assert_eq!(chosen.files, [file("Model-Q4_K_M.gguf", 400)]);
        let chosen = choose(
            "o/r",
            &files,
            &Choice {
                quant: Some("q8_0".into()),
                mmproj: true,
                ..Choice::default()
            },
        )
        .unwrap();
        let paths: Vec<_> = chosen.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "Model-Q8_0-00001-of-00002.gguf",
                "Model-Q8_0-00002-of-00002.gguf",
                "mmproj-Model-F16.gguf"
            ]
        );
        // Naming one part brings the whole set.
        let by_file = Choice {
            file: Some("Model-Q8_0-00002-of-00002.gguf".into()),
            ..Choice::default()
        };
        assert_eq!(choose("o/r", &files, &by_file).unwrap().files.len(), 2);

        let missing = Choice {
            quant: Some("Q2_K".into()),
            ..Choice::default()
        };
        let error = choose("o/r", &files, &missing).unwrap_err();
        assert!(matches!(error, HfError::NoQuant { .. }), "{error}");
        assert!(error.to_string().contains("Q4_K_M, Q8_0"), "{error}");
    }

    #[test]
    fn a_single_gguf_is_the_default_whatever_its_quant() {
        let files = [file("tiny-F16.gguf", 10)];
        assert_eq!(
            choose("o/r", &files, &Choice::default()).unwrap().name,
            "F16"
        );
    }

    #[test]
    fn safetensors_repos_take_weights_config_and_tokenizer_only() {
        let files = [
            file("config.json", 1),
            file("generation_config.json", 1),
            file("model-00001-of-00002.safetensors", 100),
            file("model-00002-of-00002.safetensors", 100),
            file("model.safetensors.index.json", 1),
            file("tokenizer.json", 1),
            file("tokenizer.model", 1),
            file("pytorch_model.bin", 200),
            file("original/consolidated.00.pth", 200),
            file("README.md", 1),
        ];
        let chosen = choose("Qwen/Qwen3-8B", &files, &Choice::default()).unwrap();
        assert_eq!(chosen.weights, Weights::Safetensors);
        assert_eq!(chosen.model_id("Qwen/Qwen3-8B"), "Qwen3-8B");
        let paths: Vec<_> = chosen.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "config.json",
                "generation_config.json",
                "model-00001-of-00002.safetensors",
                "model-00002-of-00002.safetensors",
                "model.safetensors.index.json",
                "tokenizer.json",
                "tokenizer.model"
            ]
        );
        let error = choose("o/r", &[file("README.md", 1)], &Choice::default()).unwrap_err();
        assert!(matches!(error, HfError::NoWeights { .. }), "{error}");
    }

    #[test]
    fn repo_ids_and_paths_are_checked() {
        assert!(check_repo("Qwen/Qwen3-8B-GGUF").is_ok());
        for bad in ["qwen", "a/b/c", "../x", "a/..", "a b/c", "/x"] {
            assert!(check_repo(bad).is_err(), "{bad}");
        }
        for bad in ["../x.gguf", "a//b", "C:/x", "a\\b", "/abs", "a/./b"] {
            assert!(check_path("o/r", bad).is_err(), "{bad}");
        }
        assert_eq!(encode("refs/pr/1"), "refs%2Fpr%2F1");
    }

    #[test]
    fn the_token_never_shows_in_debug_output() {
        let hub = Hub::new("http://x", Some("hf_secret".into()));
        assert!(!format!("{hub:?}").contains("hf_secret"));
    }

    #[tokio::test]
    async fn lists_and_downloads_a_split_gguf_with_checksums() {
        let part1 = crate::gguf::fixture::llama("M");
        let part2 = vec![2u8; 1000];
        let (url, _) = serve(repo(&[
            ("M-Q4_K_M-00001-of-00002.gguf", &part1),
            ("M-Q4_K_M-00002-of-00002.gguf", &part2),
            ("M-Q8_0.gguf", b"eight"),
        ]))
        .await;
        let hub = Hub::new(&url, None);
        let files = hub.files("o/r", "main").await.unwrap();
        assert_eq!(files.len(), 3, "directories are left out");
        assert!(files.iter().all(|file| file.sha256.is_some()));
        let chosen = choose("o/r", &files, &Choice::default()).unwrap();
        assert_eq!(chosen.files.len(), 2);

        let dir = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let paths = hub
            .download("o/r", "main", &chosen.files, dir.path(), |p| {
                seen.push((p.file.to_string(), p.done, p.total));
            })
            .await
            .unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), part1);
        assert_eq!(std::fs::read(&paths[1]).unwrap(), part2);
        let total = (part1.len() + part2.len()) as u64;
        assert_eq!(seen.last().unwrap().1, total);
        assert!(
            seen.iter()
                .all(|(_, done, all)| done <= all && *all == total)
        );
        // The scanner finds the set as one model, under the id the variant predicted.
        let scan = crate::scan::scan(&[dir.path().to_path_buf()]);
        assert_eq!(scan.models.len(), 1, "{:?}", scan.skipped);
        assert_eq!(scan.models[0].parts, 2);
        assert_eq!(scan.models[0].id, chosen.model_id("o/r"));
    }

    #[tokio::test]
    async fn interrupted_downloads_resume_from_the_part_file() {
        let body: Vec<u8> = (0..5000u32).map(|n| (n % 251) as u8).collect();
        let mut fake = repo(&[("m-Q4_K_M.gguf", &body)]);
        fake.truncate_at.insert("m-Q4_K_M.gguf".into(), 2000);
        let (url, shared) = serve(fake).await;
        let hub = Hub::new(&url, None);
        let files = hub.files("o/r", "main").await.unwrap();
        let dir = tempfile::tempdir().unwrap();

        let error = hub
            .download("o/r", "main", &files, dir.path(), |_| {})
            .await
            .unwrap_err();
        assert!(
            matches!(error, HfError::Size { actual: 2000, .. }),
            "{error}"
        );
        let part = dir.path().join("m-Q4_K_M.gguf.part");
        assert_eq!(std::fs::metadata(&part).unwrap().len(), 2000);

        shared.repo.lock().unwrap().truncate_at.clear();
        let paths = hub
            .download("o/r", "main", &files, dir.path(), |_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), body);
        assert!(!part.exists());
        let ranges = shared.ranges.lock().unwrap();
        assert_eq!(ranges[0].1, None);
        assert_eq!(ranges[1].1.as_deref(), Some("bytes=2000-"));
    }

    #[tokio::test]
    async fn checksum_mismatches_are_rejected_and_nothing_is_left_behind() {
        let mut fake = repo(&[("m-Q4_K_M.gguf", b"tampered weights")]);
        fake.bad_checksums.push("m-Q4_K_M.gguf".into());
        let (url, _) = serve(fake).await;
        let hub = Hub::new(&url, None);
        let files = hub.files("o/r", "main").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let error = hub
            .download("o/r", "main", &files, dir.path(), |_| {})
            .await
            .unwrap_err();
        assert!(matches!(error, HfError::Checksum { .. }), "{error}");
        assert!(!dir.path().join("m-Q4_K_M.gguf").exists());
        assert!(!dir.path().join("m-Q4_K_M.gguf.part").exists());
    }

    #[tokio::test]
    async fn gated_repos_ask_for_a_token_and_accept_one() {
        let mut fake = repo(&[("m-Q4_K_M.gguf", b"weights")]);
        fake.token = Some("hf_good".into());
        let (url, _) = serve(fake).await;

        let error = Hub::new(&url, None).files("o/r", "main").await.unwrap_err();
        assert!(matches!(error, HfError::TokenNeeded { .. }), "{error}");
        assert!(error.to_string().contains("HF_TOKEN"), "{error}");

        let error = Hub::new(&url, Some("hf_bad".into()))
            .files("o/r", "main")
            .await
            .unwrap_err();
        assert!(matches!(error, HfError::TokenRefused { .. }), "{error}");
        assert!(!error.to_string().contains("hf_bad"));

        let hub = Hub::new(&url, Some("hf_good".into()));
        let files = hub.files("o/r", "main").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        hub.download("o/r", "main", &files, dir.path(), |_| {})
            .await
            .unwrap();
        assert!(dir.path().join("m-Q4_K_M.gguf").is_file());
    }
}
