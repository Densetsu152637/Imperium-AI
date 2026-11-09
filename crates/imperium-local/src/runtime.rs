//! The llama.cpp runtime: where it is installed, which `llama-server` models are loaded with,
//! and installing llama.cpp's own release build for this machine from GitHub, verified against
//! the digests GitHub publishes for each release asset.
//!
//! Which `llama-server` is used, in order: `runtime.llamacpp.custom_path`; an installed build
//! (the first of [`packs_for`] that is installed); a `llama-server` already on this machine that
//! starts, takes the flags Imperium passes, and preferably sees the GPU ([`resolve`]).

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use futures_util::StreamExt;
use imperium_config::LlamaCppSettings;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::hardware::{Hardware, platform};
use crate::runtime_check::{self, RuntimeCheck};

/// Where llama.cpp's releases are read from.
pub const GITHUB_API: &str = "https://api.github.com";
/// The repository llama.cpp publishes its builds from.
pub const LLAMA_CPP_REPOSITORY: &str = "ggml-org/llama.cpp";
const USER_AGENT: &str = concat!("imperium/", env!("CARGO_PKG_VERSION"));

/// The llama.cpp release installed, pinned in `runtimes/llama-cpp.env`.
pub fn llama_cpp_ref() -> &'static str {
    include_str!("../../../runtimes/llama-cpp.env")
        .lines()
        .find_map(|line| line.strip_prefix("LLAMA_CPP_REF="))
        .map(str::trim)
        .expect("runtimes/llama-cpp.env sets LLAMA_CPP_REF")
}

/// The folder name an installed build lives in.
pub fn archive_name(pack: &str) -> String {
    format!("imperium-runtime-llama.cpp-{pack}")
}

/// The folder a build (`<os>-<arch>-<backend>`) is installed in.
pub fn pack_dir(data_dir: &Path, pack: &str) -> PathBuf {
    runtimes_root(data_dir).join(archive_name(pack))
}

fn runtimes_root(data_dir: &Path) -> PathBuf {
    data_dir
        .join("runtimes")
        .join("llama.cpp")
        .join(llama_cpp_ref())
}

/// `llama-server` inside an installed build.
pub fn server_in(pack_dir: &Path) -> PathBuf {
    pack_dir
        .join("bin")
        .join(format!("llama-server{}", std::env::consts::EXE_SUFFIX))
}

/// The build to install: the configured backend on this platform, or for `auto` the one that
/// suits `hardware` best.
pub fn pack_for(backend: &str, hardware: &Hardware) -> String {
    packs_for(backend, hardware).swap_remove(0)
}

/// The builds to try, best first: for `auto` every backend of [`Hardware::backend_chain`],
/// otherwise just the configured one.
pub fn packs_for(backend: &str, hardware: &Hardware) -> Vec<String> {
    let platform = platform();
    match backend {
        "auto" | "" => hardware
            .backend_chain()
            .into_iter()
            .map(|backend| format!("{platform}-{backend}"))
            .collect(),
        backend => vec![format!("{platform}-{backend}")],
    }
}

/// The `llama-server` the settings name and its source: `custom` for `custom_path`, otherwise
/// the first installed build of [`packs_for`], or the best one's (missing) path when none is.
pub fn server_path(
    settings: &LlamaCppSettings,
    data_dir: &Path,
    hardware: &Hardware,
) -> (String, PathBuf) {
    server_for(
        settings.custom_path.as_deref(),
        &settings.backend,
        data_dir,
        hardware,
    )
}

fn server_for(
    custom_path: Option<&Path>,
    backend: &str,
    data_dir: &Path,
    hardware: &Hardware,
) -> (String, PathBuf) {
    if let Some(path) = custom_path {
        return (SOURCE_CUSTOM.into(), path.to_path_buf());
    }
    let packs = packs_for(backend, hardware);
    packs
        .iter()
        .map(|pack| (pack.clone(), server_in(&pack_dir(data_dir, pack))))
        .find(|(_, server)| server.is_file())
        .unwrap_or_else(|| {
            let pack = packs[0].clone();
            let server = server_in(&pack_dir(data_dir, &pack));
            (pack, server)
        })
}

/// Where the `llama-server` in use comes from.
pub const SOURCE_CUSTOM: &str = "custom";
pub const SOURCE_PACK: &str = "pack";
pub const SOURCE_DETECTED: &str = "detected";

/// The `llama-server` to load models with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// [`SOURCE_CUSTOM`], [`SOURCE_PACK`], or [`SOURCE_DETECTED`].
    pub source: &'static str,
    /// The installed build (or the one to install), such as `windows-x64-cuda13`.
    pub pack: String,
    pub server: PathBuf,
    /// The check made while choosing a found server; `None` when no check was needed.
    pub check: Option<RuntimeCheck>,
}

impl Resolved {
    /// The label the daemon shows for the server: the build, or `custom` or `detected`.
    pub fn label(&self) -> String {
        match self.source {
            SOURCE_PACK => self.pack.clone(),
            source => source.to_string(),
        }
    }
}

/// The `llama-server` to use: [`server_path`] when that is a custom path or an installed
/// build, otherwise the best `llama-server` found on this machine ([`detect`]); when there is
/// none, the build's missing path.
pub async fn resolve(
    settings: &LlamaCppSettings,
    data_dir: &Path,
    hardware: &Hardware,
    home: Option<&Path>,
) -> Resolved {
    let (pack, server) = server_path(settings, data_dir, hardware);
    if pack == SOURCE_CUSTOM {
        return Resolved {
            source: SOURCE_CUSTOM,
            pack: pack_for(&settings.backend, hardware),
            server,
            check: None,
        };
    }
    if settings.find_existing
        && !server.is_file()
        && let Some((found, check)) = detect(runtime_check::find_existing(home), hardware).await
    {
        return Resolved {
            source: SOURCE_DETECTED,
            pack,
            server: found,
            check: Some(check),
        };
    }
    Resolved {
        source: SOURCE_PACK,
        pack,
        server,
        check: None,
    }
}

/// The best of `candidates`: each is checked, those that do not start or lack a flag Imperium
/// passes are dropped, and of the rest [`best_found`] picks.
pub async fn detect(
    candidates: Vec<PathBuf>,
    hardware: &Hardware,
) -> Option<(PathBuf, RuntimeCheck)> {
    let mut usable = Vec::new();
    for candidate in candidates {
        let check = runtime_check::check(&candidate, hardware).await;
        if !check.runs {
            tracing::debug!(server = %candidate.display(), "found llama-server does not start");
            continue;
        }
        match runtime_check::missing_flags(&candidate).await {
            Some(missing) if missing.is_empty() => usable.push((candidate, check)),
            missing => {
                tracing::info!(
                    server = %candidate.display(),
                    ?missing,
                    "found llama-server is too old to use"
                );
            }
        }
    }
    let best = best_found(&usable)?;
    Some(usable.swap_remove(best))
}

/// Which of the usable servers found to use: the first that sees the GPU, else the first that
/// could not say, else the first.
pub fn best_found(found: &[(PathBuf, RuntimeCheck)]) -> Option<usize> {
    let rank = |check: &RuntimeCheck| match check.gpu_visible {
        Some(true) => 0,
        None => 1,
        Some(false) => 2,
    };
    found
        .iter()
        .enumerate()
        .min_by_key(|(index, (_, check))| (rank(check), *index))
        .map(|(index, _)| index)
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not read llama.cpp release {0}: {1}")]
    Release(String, String),
    #[error("download of {0} failed: {1}")]
    Download(String, String),
    #[error("llama.cpp {release} has no build for {wanted}")]
    NoBuild { release: String, wanted: String },
    #[error("the release lists no SHA-256 digest for {0}, so it cannot be verified")]
    NoDigest(String),
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("{0} does not contain llama-server")]
    MissingServer(String),
    #[error("no llama.cpp build works on this machine: {0}")]
    NothingWorks(String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> InstallError {
    let context = context.into();
    move |error| InstallError::Io(context, error)
}

/// A GitHub release, as the REST API lists it.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

/// A release asset.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    /// The API URL, which serves the file to `Accept: application/octet-stream` (the
    /// `github.com/.../releases/download` links have returned 504 for hours at a time).
    pub url: String,
    #[serde(default)]
    pub size: u64,
    /// Such as `sha256:3c53...`.
    #[serde(default)]
    pub digest: Option<String>,
}

/// The assets a build is installed from: llama.cpp's archive, then any companions it needs
/// (the CUDA runtime libraries llama.cpp ships separately for its CUDA builds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub pack: String,
    pub assets: Vec<Asset>,
}

impl Selection {
    pub fn total_bytes(&self) -> u64 {
        self.assets.iter().map(|asset| asset.size).sum()
    }
}

/// A release asset's name read as `<llama|cudart>-...-bin-<os>[-<variant>...]-<arch>.<ext>`,
/// such as `llama-b12180-bin-win-cuda-13.5-x64.zip`.
struct AssetName<'a> {
    cudart: bool,
    os: &'a str,
    variant: Vec<&'a str>,
    arch: &'a str,
    /// Everything after `-bin-`, which a CUDA build and its runtime libraries share.
    key: &'a str,
}

fn parse_asset(name: &str) -> Option<AssetName<'_>> {
    let stem = name
        .strip_suffix(".zip")
        .or_else(|| name.strip_suffix(".tar.gz"))?;
    let (prefix, key) = stem.split_once("-bin-")?;
    let cudart = prefix.starts_with("cudart-");
    if !cudart && !prefix.starts_with("llama-") {
        return None;
    }
    let tokens: Vec<&str> = key.split('-').collect();
    let (os, rest) = tokens.split_first()?;
    let (arch, variant) = rest.split_last()?;
    Some(AssetName {
        cudart,
        os,
        variant: variant.to_vec(),
        arch,
        key,
    })
}

fn os_matches(token: &str, os: &str) -> bool {
    match os {
        "windows" => token == "win",
        "linux" => token == "ubuntu" || token == "linux",
        "macos" => token == "macos",
        _ => false,
    }
}

fn backend_matches(os: &str, arch: &str, variant: &[&str], backend: &str) -> bool {
    match (backend, variant) {
        ("cpu", [] | ["cpu"]) | ("vulkan", ["vulkan"]) | ("rocm", ["rocm", _]) => true,
        ("metal", []) => os == "macos" && arch == "arm64",
        ("cuda12", ["cuda", version]) => version.starts_with("12."),
        ("cuda13", ["cuda", version]) => version.starts_with("13."),
        _ => false,
    }
}

/// The assets of `release` that install `pack` (`<os>-<arch>-<backend>`), or `None` when the
/// release has no such build.
pub fn select(release: &Release, pack: &str) -> Option<Selection> {
    let mut parts = pack.splitn(3, '-');
    let (os, arch, backend) = (parts.next()?, parts.next()?, parts.next()?);
    let main = release.assets.iter().find(|asset| {
        parse_asset(&asset.name).is_some_and(|name| {
            !name.cudart
                && os_matches(name.os, os)
                && name.arch == arch
                && backend_matches(os, arch, &name.variant, backend)
        })
    })?;
    let key = parse_asset(&main.name)?.key;
    let mut assets = vec![main.clone()];
    assets.extend(
        release
            .assets
            .iter()
            .filter(|asset| {
                parse_asset(&asset.name).is_some_and(|name| name.cudart && name.key == key)
            })
            .cloned(),
    );
    Some(Selection {
        pack: pack.to_string(),
        assets,
    })
}

/// The SHA-256 an asset must have, from its `digest`.
pub fn expected_sha256(asset: &Asset) -> Result<String, InstallError> {
    asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase)
        .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| InstallError::NoDigest(asset.name.clone()))
}

/// Reads the release `tag` from `api` (normally [`GITHUB_API`]).
pub async fn fetch_release(
    http: &reqwest::Client,
    api: &str,
    tag: &str,
) -> Result<Release, InstallError> {
    let url = format!("{api}/repos/{LLAMA_CPP_REPOSITORY}/releases/tags/{tag}");
    let fail = |error: String| InstallError::Release(tag.to_string(), error);
    let response = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| fail(error.to_string()))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| fail(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| fail(error.to_string()))
}

/// Progress: the build being installed, bytes downloaded so far, and its assets' total size.
pub type Progress<'a> = &'a (dyn Fn(&str, u64, u64) + Send + Sync);

/// Downloads `asset` to `file`, checking it against its digest as it arrives; `offset` is what
/// earlier assets of the same build already downloaded.
async fn download(
    http: &reqwest::Client,
    asset: &Asset,
    file: &Path,
    report: &(dyn Fn(u64) + Send + Sync),
    offset: u64,
) -> Result<(), InstallError> {
    let expected = expected_sha256(asset)?;
    let fail = |error: String| InstallError::Download(asset.name.clone(), error);
    let response = http
        .get(&asset.url)
        .header("Accept", "application/octet-stream")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| fail(error.to_string()))?;
    let mut out = std::fs::File::create(file).map_err(io(file.display().to_string()))?;
    let mut hasher = Sha256::new();
    let mut body = response.bytes_stream();
    let mut done = 0u64;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| fail(error.to_string()))?;
        done += chunk.len() as u64;
        report(offset + done);
        hasher.update(&chunk);
        out.write_all(&chunk)
            .map_err(io(file.display().to_string()))?;
    }
    drop(out);
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(file);
        return Err(InstallError::Checksum {
            file: asset.name.clone(),
            expected,
            actual,
        });
    }
    Ok(())
}

/// Downloads and verifies `selection`'s assets and installs them as its build under
/// `data_dir`, replacing an earlier install only once everything is in place. Returns the
/// path of `llama-server`.
pub async fn install_selection(
    http: &reqwest::Client,
    data_dir: &Path,
    selection: &Selection,
    progress: Progress<'_>,
) -> Result<PathBuf, InstallError> {
    let pack = selection.pack.as_str();
    let root = runtimes_root(data_dir);
    let staging = root.join(format!(".staging-{pack}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(io(staging.display().to_string()))?;
    let result = stage(http, selection, &staging, progress).await;
    let installed = match result {
        Ok(staged) => {
            let target = pack_dir(data_dir, pack);
            let _ = std::fs::remove_dir_all(&target);
            std::fs::rename(&staged, &target)
                .map_err(io(target.display().to_string()))
                .map(|()| server_in(&target))
        }
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_dir_all(&staging);
    installed
}

/// Downloads and unpacks into `staging`; returns the build's folder there.
async fn stage(
    http: &reqwest::Client,
    selection: &Selection,
    staging: &Path,
    progress: Progress<'_>,
) -> Result<PathBuf, InstallError> {
    let pack = selection.pack.as_str();
    let total = selection.total_bytes();
    let report = |done: u64| progress(pack, done, total);
    report(0);
    let mut offset = 0;
    let mut unpacked = Vec::new();
    for (index, asset) in selection.assets.iter().enumerate() {
        let archive = staging.join(&asset.name);
        download(http, asset, &archive, &report, offset).await?;
        offset += asset.size;
        let into = staging.join(format!("unpacked-{index}"));
        let from = archive.clone();
        let target = into.clone();
        tokio::task::spawn_blocking(move || extract(&from, &target))
            .await
            .map_err(|error| {
                InstallError::Io(asset.name.clone(), std::io::Error::other(error))
            })??;
        let _ = std::fs::remove_file(&archive);
        unpacked.push(into);
    }
    let main = &selection.assets[0];
    let server_dir = find_server_dir(&unpacked[0], 3)
        .ok_or_else(|| InstallError::MissingServer(main.name.clone()))?;
    let built = staging.join(archive_name(pack));
    let bin = built.join("bin");
    std::fs::create_dir_all(&built).map_err(io(built.display().to_string()))?;
    std::fs::rename(&server_dir, &bin).map_err(io(bin.display().to_string()))?;
    // The CUDA runtime libraries go beside llama-server, where it looks for them.
    for companion in &unpacked[1..] {
        merge_into(&content_root(companion), &bin)?;
    }
    let manifest = serde_json::json!({
        "llama_cpp_ref": llama_cpp_ref(),
        "repository": LLAMA_CPP_REPOSITORY,
        "assets": selection.assets.iter().map(|asset| serde_json::json!({
            "name": asset.name,
            "digest": asset.digest,
        })).collect::<Vec<_>>(),
    });
    let manifest_path = built.join("imperium-runtime.json");
    std::fs::write(&manifest_path, manifest.to_string())
        .map_err(io(manifest_path.display().to_string()))?;
    Ok(built)
}

/// Unpacks a `.zip` or `.tar.gz`; both refuse entries that would land outside `into`.
fn extract(archive: &Path, into: &Path) -> Result<(), InstallError> {
    let context = || format!("unpacking {}", archive.display());
    let file = std::fs::File::open(archive).map_err(io(archive.display().to_string()))?;
    std::fs::create_dir_all(into).map_err(io(into.display().to_string()))?;
    if archive.extension().is_some_and(|ext| ext == "zip") {
        zip::ZipArchive::new(file)
            .and_then(|mut zip| zip.extract(into))
            .map_err(|error| InstallError::Io(context(), std::io::Error::other(error)))
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(file))
            .unpack(into)
            .map_err(io(context()))
    }
}

/// The folder holding `llama-server`, searched `depth` folders deep.
fn find_server_dir(dir: &Path, depth: usize) -> Option<PathBuf> {
    let exe = format!("llama-server{}", std::env::consts::EXE_SUFFIX);
    if dir.join(&exe).is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs.iter()
        .find_map(|child| find_server_dir(child, depth - 1))
}

/// `dir`, or the single folder it holds (repeatedly), so an archive's top folder is skipped.
fn content_root(dir: &Path) -> PathBuf {
    let mut dir = dir.to_path_buf();
    loop {
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        match entries.as_slice() {
            [only] if only.is_dir() => dir = only.clone(),
            _ => return dir,
        }
    }
}

/// Moves everything in `from` into `into`, replacing what is there.
fn merge_into(from: &Path, into: &Path) -> Result<(), InstallError> {
    for entry in std::fs::read_dir(from).map_err(io(from.display().to_string()))? {
        let entry = entry.map_err(io(from.display().to_string()))?;
        let target = into.join(entry.file_name());
        if target.is_dir() {
            let _ = std::fs::remove_dir_all(&target);
        } else if target.exists() {
            let _ = std::fs::remove_file(&target);
        }
        std::fs::rename(entry.path(), &target).map_err(io(target.display().to_string()))?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether an installed build is good enough to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It runs and, where the machine has a GPU and the build is for one, sees it.
    Good,
    /// It runs but does not see the GPU; kept only if no later build does better.
    NoGpu(String),
    /// It does not run here.
    Broken(String),
}

/// The verdict on `pack` from its [`RuntimeCheck`].
pub fn verdict(pack: &str, check: &RuntimeCheck, hardware: &Hardware) -> Verdict {
    let why = || {
        check
            .problem
            .clone()
            .unwrap_or_else(|| check.summary.clone())
    };
    if !check.runs {
        return Verdict::Broken(why());
    }
    let has_gpu = !hardware.gpus.is_empty() || hardware.unified_memory;
    if pack.ends_with("-cpu") || !has_gpu || check.gpu_visible != Some(false) {
        Verdict::Good
    } else {
        Verdict::NoGpu(why())
    }
}

/// Checks an installed build: given the build and its `llama-server`.
pub type Verify<'a> =
    &'a (dyn Fn(String, PathBuf) -> Pin<Box<dyn Future<Output = Verdict> + Send>> + Send + Sync);

/// What [`install_first_working`] installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub pack: String,
    pub server: PathBuf,
    /// The builds passed over and why, in order.
    pub skipped: Vec<String>,
}

/// Installs the first of `packs` (best first) that `release` has and that `verify` accepts.
/// A build that does not run is removed and the next one tried; one that runs without seeing
/// the GPU is kept only when no later build does better. A failed download stops at once.
pub async fn install_first_working(
    http: &reqwest::Client,
    release: &Release,
    data_dir: &Path,
    packs: &[String],
    verify: Verify<'_>,
    progress: Progress<'_>,
) -> Result<Installed, InstallError> {
    let mut skipped = Vec::new();
    let mut fallback: Option<(String, PathBuf)> = None;
    let mut tried = false;
    for pack in packs {
        let Some(selection) = select(release, pack) else {
            skipped.push(format!(
                "{pack}: llama.cpp {} has no such build",
                release.tag_name
            ));
            continue;
        };
        tried = true;
        let server = install_selection(http, data_dir, &selection, progress).await?;
        match verify(pack.clone(), server.clone()).await {
            Verdict::Good => {
                if let Some((earlier, _)) = fallback.take() {
                    let _ = std::fs::remove_dir_all(pack_dir(data_dir, &earlier));
                }
                return Ok(Installed {
                    pack: pack.clone(),
                    server,
                    skipped,
                });
            }
            Verdict::NoGpu(why) => {
                skipped.push(format!("{pack}: {why}"));
                if fallback.is_none() {
                    fallback = Some((pack.clone(), server));
                } else {
                    let _ = std::fs::remove_dir_all(pack_dir(data_dir, pack));
                }
            }
            Verdict::Broken(why) => {
                skipped.push(format!("{pack}: {why}"));
                let _ = std::fs::remove_dir_all(pack_dir(data_dir, pack));
            }
        }
    }
    match fallback {
        Some((pack, server)) => Ok(Installed {
            pack,
            server,
            skipped,
        }),
        None if !tried => Err(InstallError::NoBuild {
            release: release.tag_name.clone(),
            wanted: packs.join(", "),
        }),
        None => Err(InstallError::NothingWorks(skipped.join("; "))),
    }
}

/// Installs llama.cpp's build for this machine: reads the pinned release from `api` and
/// installs the first of `packs` that runs here ([`install_first_working`], checked with
/// [`runtime_check::check`]).
pub async fn install(
    http: &reqwest::Client,
    api: &str,
    data_dir: &Path,
    packs: &[String],
    hardware: &Hardware,
    progress: Progress<'_>,
) -> Result<Installed, InstallError> {
    let release = fetch_release(http, api, llama_cpp_ref()).await?;
    let checking = hardware.clone();
    let verify = move |pack: String, server: PathBuf| {
        let hardware = checking.clone();
        Box::pin(async move {
            let check = runtime_check::check(&server, &hardware).await;
            verdict(&pack, &check, &hardware)
        }) as Pin<Box<dyn Future<Output = Verdict> + Send>>
    };
    install_first_working(http, &release, data_dir, packs, &verify, progress).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::hardware::Gpu;
    use axum::Router;
    use axum::routing::get as get_route;

    const REF: &str = "b12180";

    /// The asset names of llama.cpp b12180 that matter here.
    const NAMES: &[&str] = &[
        "cudart-llama-b12180-bin-ubuntu-cuda-12.8-x64.tar.gz",
        "cudart-llama-b12180-bin-ubuntu-cuda-13.5-x64.tar.gz",
        "cudart-llama-bin-win-cuda-12.4-x64.zip",
        "cudart-llama-bin-win-cuda-13.5-arm64.zip",
        "cudart-llama-bin-win-cuda-13.5-x64.zip",
        "llama-b12180-bin-android-arm64.tar.gz",
        "llama-b12180-bin-linux-arm64-snapdragon.tar.gz",
        "llama-b12180-bin-macos-arm64.tar.gz",
        "llama-b12180-bin-macos-x64.tar.gz",
        "llama-b12180-bin-ubuntu-26.04-s390x.tar.gz",
        "llama-b12180-bin-ubuntu-arm64.tar.gz",
        "llama-b12180-bin-ubuntu-cuda-12.8-x64.tar.gz",
        "llama-b12180-bin-ubuntu-cuda-13.5-x64.tar.gz",
        "llama-b12180-bin-ubuntu-rocm-10.0-x64.tar.gz",
        "llama-b12180-bin-ubuntu-vulkan-x64.tar.gz",
        "llama-b12180-bin-ubuntu-x64.tar.gz",
        "llama-b12180-bin-win-cpu-arm64.zip",
        "llama-b12180-bin-win-cpu-x64.zip",
        "llama-b12180-bin-win-cuda-12.4-x64.zip",
        "llama-b12180-bin-win-cuda-13.5-arm64.zip",
        "llama-b12180-bin-win-cuda-13.5-x64.zip",
        "llama-b12180-bin-win-vulkan-x64.zip",
        "llama-b12180-ui.tar.gz",
        "llama-b12180-xcframework.zip",
    ];

    fn fake_release() -> Release {
        let assets: Vec<serde_json::Value> = NAMES
            .iter()
            .enumerate()
            .map(|(id, name)| {
                serde_json::json!({
                    "name": name,
                    "url": format!("https://api.github.com/repos/ggml-org/llama.cpp/releases/assets/{id}"),
                    "size": 100 + id,
                    "digest": format!("sha256:{}", "a".repeat(64)),
                    "browser_download_url": format!("https://github.com/x/{name}"),
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({ "tag_name": REF, "assets": assets })).unwrap()
    }

    fn names(selection: Option<Selection>) -> Vec<String> {
        selection
            .map(|selection| {
                selection
                    .assets
                    .into_iter()
                    .map(|asset| asset.name)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn each_platform_and_backend_gets_its_build_and_cuda_runtime() {
        let release = fake_release();
        let cases: &[(&str, &[&str])] = &[
            (
                "windows-x64-cuda13",
                &[
                    "llama-b12180-bin-win-cuda-13.5-x64.zip",
                    "cudart-llama-bin-win-cuda-13.5-x64.zip",
                ],
            ),
            (
                "windows-x64-cuda12",
                &[
                    "llama-b12180-bin-win-cuda-12.4-x64.zip",
                    "cudart-llama-bin-win-cuda-12.4-x64.zip",
                ],
            ),
            (
                "windows-arm64-cuda13",
                &[
                    "llama-b12180-bin-win-cuda-13.5-arm64.zip",
                    "cudart-llama-bin-win-cuda-13.5-arm64.zip",
                ],
            ),
            (
                "windows-x64-vulkan",
                &["llama-b12180-bin-win-vulkan-x64.zip"],
            ),
            ("windows-x64-cpu", &["llama-b12180-bin-win-cpu-x64.zip"]),
            ("windows-arm64-cpu", &["llama-b12180-bin-win-cpu-arm64.zip"]),
            (
                "linux-x64-cuda13",
                &[
                    "llama-b12180-bin-ubuntu-cuda-13.5-x64.tar.gz",
                    "cudart-llama-b12180-bin-ubuntu-cuda-13.5-x64.tar.gz",
                ],
            ),
            (
                "linux-x64-cuda12",
                &[
                    "llama-b12180-bin-ubuntu-cuda-12.8-x64.tar.gz",
                    "cudart-llama-b12180-bin-ubuntu-cuda-12.8-x64.tar.gz",
                ],
            ),
            (
                "linux-x64-vulkan",
                &["llama-b12180-bin-ubuntu-vulkan-x64.tar.gz"],
            ),
            (
                "linux-x64-rocm",
                &["llama-b12180-bin-ubuntu-rocm-10.0-x64.tar.gz"],
            ),
            ("linux-x64-cpu", &["llama-b12180-bin-ubuntu-x64.tar.gz"]),
            ("linux-arm64-cpu", &["llama-b12180-bin-ubuntu-arm64.tar.gz"]),
            (
                "macos-arm64-metal",
                &["llama-b12180-bin-macos-arm64.tar.gz"],
            ),
            ("macos-arm64-cpu", &["llama-b12180-bin-macos-arm64.tar.gz"]),
            ("macos-x64-cpu", &["llama-b12180-bin-macos-x64.tar.gz"]),
        ];
        for (pack, expected) in cases {
            assert_eq!(names(select(&release, pack)), *expected, "{pack}");
        }
        for missing in [
            "linux-arm64-cuda12",
            "linux-arm64-vulkan",
            "macos-x64-metal",
            "windows-x64-metal",
            "windows-x64-rocm",
        ] {
            assert_eq!(select(&release, missing), None, "{missing}");
        }
    }

    #[test]
    fn digests_must_be_sha256() {
        let mut asset = fake_release().assets.remove(0);
        assert_eq!(expected_sha256(&asset).unwrap(), "a".repeat(64));
        asset.digest = Some(format!("sha256:{}", "A".repeat(64)));
        assert_eq!(expected_sha256(&asset).unwrap(), "a".repeat(64));
        for bad in [
            None,
            Some("md5:abc".to_string()),
            Some("sha256:xyz".to_string()),
        ] {
            asset.digest = bad;
            assert!(matches!(
                expected_sha256(&asset),
                Err(InstallError::NoDigest(_))
            ));
        }
    }

    fn gpu_machine() -> Hardware {
        Hardware {
            gpus: vec![Gpu {
                name: "NVIDIA GeForce RTX 5080".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: 16 << 30,
                vram_free_bytes: 12 << 30,
                driver: Some("581.57".into()),
            }],
            ram_total_bytes: 64 << 30,
            ram_available_bytes: 32 << 30,
            cpu_cores: 16,
            unified_memory: false,
            overridden: false,
        }
    }

    fn checked(runs: bool, gpu_visible: Option<bool>) -> RuntimeCheck {
        RuntimeCheck {
            runs,
            version: None,
            devices: Vec::new(),
            gpu_visible,
            problem: (!runs).then(|| "missing DLL".to_string()),
            summary: String::new(),
        }
    }

    #[test]
    fn found_servers_that_see_the_gpu_come_first() {
        let found = vec![
            (PathBuf::from("a"), checked(true, Some(false))),
            (PathBuf::from("b"), checked(true, None)),
            (PathBuf::from("c"), checked(true, Some(true))),
            (PathBuf::from("d"), checked(true, Some(true))),
        ];
        assert_eq!(best_found(&found), Some(2));
        assert_eq!(best_found(&found[..2]), Some(1));
        assert_eq!(best_found(&found[..1]), Some(0));
        assert_eq!(best_found(&[]), None);
    }

    #[test]
    fn verdicts_need_the_gpu_unless_the_build_is_for_the_cpu() {
        let machine = gpu_machine();
        let pack = "windows-x64-cuda13";
        assert_eq!(
            verdict(pack, &checked(true, Some(true)), &machine),
            Verdict::Good
        );
        assert_eq!(verdict(pack, &checked(true, None), &machine), Verdict::Good);
        assert!(matches!(
            verdict(pack, &checked(true, Some(false)), &machine),
            Verdict::NoGpu(_)
        ));
        assert!(matches!(
            verdict(pack, &checked(false, None), &machine),
            Verdict::Broken(_)
        ));
        assert_eq!(
            verdict("windows-x64-cpu", &checked(true, Some(false)), &machine),
            Verdict::Good
        );
        let mut no_gpu = machine;
        no_gpu.gpus.clear();
        assert_eq!(
            verdict(pack, &checked(true, Some(false)), &no_gpu),
            Verdict::Good
        );
    }

    #[test]
    fn installed_builds_are_used_in_fallback_order() {
        let data = tempfile::tempdir().unwrap();
        let machine = gpu_machine();
        let packs = packs_for("auto", &machine);
        let (pack, server) = server_for(None, "auto", data.path(), &machine);
        assert_eq!(pack, packs[0]);
        assert!(!server.is_file());
        // Only the Vulkan build is installed: it is the one used.
        let vulkan = format!("{}-vulkan", platform());
        let installed = server_in(&pack_dir(data.path(), &vulkan));
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "binary").unwrap();
        assert_eq!(
            server_for(None, "auto", data.path(), &machine),
            (vulkan, installed)
        );
        let custom = Path::new("/opt/llama-server");
        assert_eq!(
            server_for(Some(custom), "auto", data.path(), &machine).0,
            "custom"
        );
    }

    /// An archive holding `files` (name, contents), zipped or tarred by `name`'s extension.
    fn archive(name: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        if name.ends_with(".zip") {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            for (path, contents) in files {
                zip.start_file(*path, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(contents).unwrap();
            }
            zip.finish().unwrap().into_inner()
        } else {
            let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::fast(),
            ));
            for (path, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, path, *contents).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap()
        }
    }

    /// Serves `files` at `/assets/<name>` and a release listing them at the API path.
    async fn serve(files: Vec<(String, Vec<u8>, Option<String>)>) -> (String, Release) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut app = Router::new();
        let mut assets = Vec::new();
        for (name, body, digest) in files {
            assets.push(Asset {
                name: name.clone(),
                url: format!("{url}/assets/{name}"),
                size: body.len() as u64,
                digest: Some(
                    digest.unwrap_or_else(|| format!("sha256:{}", hex(&Sha256::digest(&body)))),
                ),
            });
            app = app.route(
                &format!("/assets/{name}"),
                get_route(move || async move { body }),
            );
        }
        let release = Release {
            tag_name: REF.into(),
            assets,
        };
        let listing = serde_json::json!({
            "tag_name": REF,
            "assets": release.assets.iter().map(|asset| serde_json::json!({
                "name": asset.name, "url": asset.url, "size": asset.size, "digest": asset.digest,
            })).collect::<Vec<_>>(),
        })
        .to_string();
        app = app.route(
            &format!("/repos/{LLAMA_CPP_REPOSITORY}/releases/tags/{REF}"),
            get_route(move || async move { listing }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await });
        (url, release)
    }

    fn exe() -> String {
        format!("llama-server{}", std::env::consts::EXE_SUFFIX)
    }

    fn accept_all(_: String, _: PathBuf) -> Pin<Box<dyn Future<Output = Verdict> + Send>> {
        Box::pin(async { Verdict::Good })
    }

    #[tokio::test]
    async fn a_cuda_build_is_installed_with_its_runtime_beside_llama_server() {
        let main = "llama-b12180-bin-win-cuda-13.5-x64.zip";
        let cudart = "cudart-llama-bin-win-cuda-13.5-x64.zip";
        let exe = exe();
        let (url, _) = serve(vec![
            (
                main.into(),
                archive(main, &[(&exe, b"server"), ("ggml-cuda.dll", b"cuda")]),
                None,
            ),
            (
                cudart.into(),
                archive(cudart, &[("cudart64_13.dll", b"rt")]),
                None,
            ),
        ])
        .await;
        let http = reqwest::Client::new();
        let release = fetch_release(&http, &url, REF).await.unwrap();
        assert_eq!(release.assets.len(), 2);
        let data = tempfile::tempdir().unwrap();
        let seen = Mutex::new(Vec::new());
        let progress = |pack: &str, done: u64, total: u64| {
            seen.lock().unwrap().push((pack.to_string(), done, total));
        };
        let installed = install_first_working(
            &http,
            &release,
            data.path(),
            &["windows-x64-cuda13".to_string()],
            &accept_all,
            &progress,
        )
        .await
        .unwrap();
        let bin = pack_dir(data.path(), "windows-x64-cuda13").join("bin");
        assert_eq!(installed.server, bin.join(&exe));
        assert!(bin.join("cudart64_13.dll").is_file());
        assert!(bin.join("ggml-cuda.dll").is_file());
        assert!(
            pack_dir(data.path(), "windows-x64-cuda13")
                .join("imperium-runtime.json")
                .is_file()
        );
        let seen = seen.lock().unwrap();
        let total = release.assets.iter().map(|asset| asset.size).sum::<u64>();
        assert_eq!(
            seen.last().unwrap(),
            &("windows-x64-cuda13".to_string(), total, total)
        );
    }

    #[tokio::test]
    async fn tarballs_lose_their_top_folder() {
        let main = "llama-b12180-bin-ubuntu-x64.tar.gz";
        let exe = exe();
        let inner = format!("llama-b12180/{exe}");
        let (url, _) = serve(vec![(
            main.into(),
            archive(
                main,
                &[(&inner, b"server"), ("llama-b12180/libllama.so", b"lib")],
            ),
            None,
        )])
        .await;
        let http = reqwest::Client::new();
        let release = fetch_release(&http, &url, REF).await.unwrap();
        let data = tempfile::tempdir().unwrap();
        let installed = install_first_working(
            &http,
            &release,
            data.path(),
            &["linux-x64-cpu".to_string()],
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap();
        assert!(installed.server.is_file());
        assert!(installed.server.with_file_name("libllama.so").is_file());
    }

    #[tokio::test]
    async fn tampered_downloads_are_refused_and_leave_the_install_alone() {
        let main = "llama-b12180-bin-win-cpu-x64.zip";
        let exe = exe();
        let http = reqwest::Client::new();
        let data = tempfile::tempdir().unwrap();
        let (_, good) = serve(vec![(main.into(), archive(main, &[(&exe, b"one")]), None)]).await;
        let packs = ["windows-x64-cpu".to_string()];
        let installed = install_first_working(
            &http,
            &good,
            data.path(),
            &packs,
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap();
        let (_, tampered) = serve(vec![(
            main.into(),
            archive(main, &[(&exe, b"two")]),
            Some(format!("sha256:{}", "0".repeat(64))),
        )])
        .await;
        let error = install_first_working(
            &http,
            &tampered,
            data.path(),
            &packs,
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::Checksum { .. }), "{error}");
        assert_eq!(std::fs::read(&installed.server).unwrap(), b"one");
        let (_, unsigned) =
            serve(vec![(main.into(), archive(main, &[(&exe, b"two")]), None)]).await;
        let mut unsigned = unsigned;
        unsigned.assets[0].digest = None;
        let error = install_first_working(
            &http,
            &unsigned,
            data.path(),
            &packs,
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::NoDigest(_)), "{error}");
    }

    #[tokio::test]
    async fn archives_without_a_server_are_refused() {
        let main = "llama-b12180-bin-win-cpu-x64.zip";
        let (_, release) = serve(vec![(
            main.into(),
            archive(main, &[("README.md", b"hi")]),
            None,
        )])
        .await;
        let data = tempfile::tempdir().unwrap();
        let error = install_first_working(
            &reqwest::Client::new(),
            &release,
            data.path(),
            &["windows-x64-cpu".to_string()],
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::MissingServer(_)), "{error}");
        assert!(!pack_dir(data.path(), "windows-x64-cpu").exists());
    }

    #[tokio::test]
    async fn builds_that_do_not_work_give_way_to_the_next() {
        let exe = exe();
        let files: Vec<(String, Vec<u8>, Option<String>)> = [
            "llama-b12180-bin-win-cuda-13.5-x64.zip",
            "llama-b12180-bin-win-vulkan-x64.zip",
            "llama-b12180-bin-win-cpu-x64.zip",
        ]
        .iter()
        .map(|name| {
            (
                name.to_string(),
                archive(name, &[(&exe, name.as_bytes())]),
                None,
            )
        })
        .collect();
        let (_, release) = serve(files).await;
        let packs: Vec<String> = ["cuda13", "cuda12", "vulkan", "cpu"]
            .iter()
            .map(|backend| format!("windows-x64-{backend}"))
            .collect();
        let data = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        // CUDA does not start; Vulkan runs without the GPU; the CPU build works.
        let verify = |pack: String, _: PathBuf| {
            Box::pin(async move {
                if pack.ends_with("cuda13") {
                    Verdict::Broken("missing DLL".into())
                } else if pack.ends_with("vulkan") {
                    Verdict::NoGpu("no GPU".into())
                } else {
                    Verdict::Good
                }
            }) as Pin<Box<dyn Future<Output = Verdict> + Send>>
        };
        let installed =
            install_first_working(&http, &release, data.path(), &packs, &verify, &|_, _, _| {})
                .await
                .unwrap();
        assert_eq!(installed.pack, "windows-x64-cpu");
        assert_eq!(installed.skipped.len(), 3, "{:?}", installed.skipped);
        assert!(installed.skipped[1].contains("no such build"));
        for gone in ["cuda13", "vulkan"] {
            assert!(!pack_dir(data.path(), &format!("windows-x64-{gone}")).exists());
        }
        // Without a working build, the one that runs (without the GPU) is kept.
        let data = tempfile::tempdir().unwrap();
        let no_cpu = |pack: String, _: PathBuf| {
            Box::pin(async move {
                if pack.ends_with("vulkan") {
                    Verdict::NoGpu("no GPU".into())
                } else {
                    Verdict::Broken("broken".into())
                }
            }) as Pin<Box<dyn Future<Output = Verdict> + Send>>
        };
        let installed =
            install_first_working(&http, &release, data.path(), &packs, &no_cpu, &|_, _, _| {})
                .await
                .unwrap();
        assert_eq!(installed.pack, "windows-x64-vulkan");
        assert!(installed.server.is_file());
        // Nothing runs: an error that says why each build was passed over.
        let none = |_: String, _: PathBuf| {
            Box::pin(async { Verdict::Broken("broken".into()) })
                as Pin<Box<dyn Future<Output = Verdict> + Send>>
        };
        let data = tempfile::tempdir().unwrap();
        let error =
            install_first_working(&http, &release, data.path(), &packs, &none, &|_, _, _| {})
                .await
                .unwrap_err();
        assert!(matches!(error, InstallError::NothingWorks(_)), "{error}");
        let error = install_first_working(
            &http,
            &release,
            data.path(),
            &["windows-x64-rocm".to_string()],
            &none,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::NoBuild { .. }), "{error}");
    }

    #[test]
    fn the_pinned_ref_is_read_from_the_env_file() {
        assert!(llama_cpp_ref().starts_with('b'), "{}", llama_cpp_ref());
    }
}
