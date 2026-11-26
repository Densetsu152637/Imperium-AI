//! The llama.cpp runtime: where it is installed, which `llama-server` models are loaded with,
//! and installing llama.cpp's own release build for this machine from GitHub, verified against
//! the digests GitHub publishes for each release asset.
//!
//! Which `llama-server` is used, in order: `runtime.llamacpp.custom_path`; an installed build
//! (the first of [`packs_for`] that is installed for the pinned release, else for the newest
//! earlier release still installed, until the pinned one is); a `llama-server` already on this
//! machine that starts, takes the flags Imperium passes, and preferably sees the GPU
//! ([`resolve`]).
//!
//! llama.cpp has no updater of its own, so the build moves with Imperium: when an update pins a
//! newer release, [`auto_install`] says to install it the next time a local model is needed;
//! the earlier build is kept (and used) until the new one has passed its checks and loaded a
//! model once ([`refs_to_remove`]).

use imperium_api::encoding::hex;
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
    releases_root(data_dir).join(llama_cpp_ref())
}

/// The folder holding one folder per installed llama.cpp release.
fn releases_root(data_dir: &Path) -> PathBuf {
    data_dir.join("runtimes").join("llama.cpp")
}

/// The build number of a release ref such as `b12180`.
pub fn ref_build(reference: &str) -> Option<u64> {
    reference.strip_prefix('b')?.parse().ok()
}

/// The build number of the pinned release ([`llama_cpp_ref`]).
pub fn pinned_build() -> Option<u64> {
    ref_build(llama_cpp_ref())
}

/// The releases installed under `data_dir`, newest first.
pub fn installed_refs(data_dir: &Path) -> Vec<String> {
    let mut refs: Vec<String> = std::fs::read_dir(releases_root(data_dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| ref_build(name).is_some())
        .collect();
    refs.sort_by_key(|name| std::cmp::Reverse(ref_build(name)));
    refs
}

/// The release a `llama-server` installed under `data_dir` belongs to, such as `b12180`;
/// `None` for one installed elsewhere.
pub fn installed_ref(data_dir: &Path, server: &Path) -> Option<String> {
    let rest = server.strip_prefix(releases_root(data_dir)).ok()?;
    let first = rest.components().next()?;
    Some(first.as_os_str().to_string_lossy().into_owned())
}

/// The earlier releases to delete: with the pinned release `current` not yet proven
/// (`current_started` false: it has not loaded a model), every one but the newest earlier
/// release, which stays as the fallback; once it has, every earlier one. Later releases (from
/// a newer Imperium run before) are left alone.
pub fn refs_to_remove(installed: &[String], current: &str, current_started: bool) -> Vec<String> {
    let current_build = ref_build(current).unwrap_or(u64::MAX);
    let mut earlier: Vec<&String> = installed
        .iter()
        .filter(|name| ref_build(name).is_some_and(|build| build < current_build))
        .collect();
    earlier.sort_by_key(|name| std::cmp::Reverse(ref_build(name)));
    let keep = usize::from(!current_started);
    earlier.into_iter().skip(keep).cloned().collect()
}

/// Deletes the earlier releases [`refs_to_remove`] names; a build whose files are in use
/// (a model still loaded from it, on Windows) is left for a later try.
pub fn remove_earlier_releases(data_dir: &Path, current_started: bool) {
    let installed = installed_refs(data_dir);
    for old in refs_to_remove(&installed, llama_cpp_ref(), current_started) {
        let dir = releases_root(data_dir).join(&old);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => tracing::info!(release = %old, "removed an earlier llama.cpp build"),
            Err(error) => {
                tracing::debug!(release = %old, %error, "earlier llama.cpp build not removed yet");
            }
        }
    }
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
    let current = llama_cpp_ref();
    // The pinned release first, then (until it is installed) the newest earlier one.
    let releases = std::iter::once(current.to_string()).chain(
        installed_refs(data_dir)
            .into_iter()
            .filter(|name| ref_build(name) < ref_build(current)),
    );
    releases
        .flat_map(|release| {
            let root = releases_root(data_dir).join(release);
            packs
                .iter()
                .map(move |pack| (pack.clone(), server_in(&root.join(archive_name(pack)))))
                .collect::<Vec<_>>()
        })
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
    let best = best_found(&usable, hardware)?;
    Some(usable.swap_remove(best))
}

/// Which of the usable servers found to use: one that sees the GPU through the backend that
/// suits `hardware` (CUDA on an NVIDIA GPU, Metal on Apple Silicon), then one that sees a GPU
/// otherwise, then one that could not say, then one that sees none; among equals the newest
/// build (by its `--version` build number), then the first found.
pub fn best_found(found: &[(PathBuf, RuntimeCheck)], hardware: &Hardware) -> Option<usize> {
    let family = device_family(hardware.backend_chain()[0]);
    let rank = |check: &RuntimeCheck| match check.gpu_visible {
        Some(true)
            if family.is_some_and(|family| {
                check.devices.iter().any(|device| {
                    device.is_gpu() && device.id.to_ascii_uppercase().starts_with(family)
                })
            }) =>
        {
            0
        }
        Some(true) => 1,
        None => 2,
        Some(false) => 3,
    };
    found
        .iter()
        .enumerate()
        .min_by_key(|(index, (_, check))| {
            (
                rank(check),
                std::cmp::Reverse(build_number(check.version.as_deref())),
                *index,
            )
        })
        .map(|(index, _)| index)
}

/// The prefix of the `--list-devices` ids a backend's GPUs have (`CUDA0`, `MTL0`, `Vulkan0`,
/// `ROCm0`); `None` for the CPU.
fn device_family(backend: &str) -> Option<&'static str> {
    match backend {
        "cuda12" | "cuda13" => Some("CUDA"),
        "metal" => Some("MTL"),
        "vulkan" => Some("VULKAN"),
        "rocm" => Some("ROCM"),
        _ => None,
    }
}

/// The llama.cpp build number in a `--version` line such as `version: 6000 (58670d1)` (or
/// `version: b6000 (...)`), comparable with a release ref's ([`ref_build`]).
pub fn build_number(version: Option<&str>) -> Option<u64> {
    let word = version?
        .trim()
        .strip_prefix("version:")?
        .split_whitespace()
        .next()?;
    word.strip_prefix('b').unwrap_or(word).parse().ok()
}

/// Whether the daemon installs llama.cpp's build for this machine by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoInstall {
    /// Keep the `llama-server` in use (or the lack of one).
    Keep,
    /// No usable `llama-server` is there: install the build.
    Missing,
    /// The one found runs on the CPU only although this machine has a GPU a build supports:
    /// install the GPU build instead.
    CpuOnly,
    /// The build in use was installed for an earlier release than Imperium now pins: install
    /// the pinned one (the earlier one stays in use until it is in place).
    Outdated,
}

impl AutoInstall {
    pub fn installs(self) -> bool {
        self != AutoInstall::Keep
    }
}

/// Whether to install llama.cpp's build without being asked, from what [`resolve`] chose
/// (`installed_ref`: the release of the build in use when it is one installed under the data
/// folder, [`installed_ref`]): never for a custom `llama-server`, with
/// `runtime.llamacpp.auto_install` off, or once an installation has been tried since the
/// daemon started (`tried`; a failed one is retried only from the app or
/// `imperium runtime install`). Otherwise when nothing usable is installed or found, when the
/// installed build is for an earlier release than the pinned one, or when the server found runs
/// on the CPU only on a machine whose build of choice ([`pack_for`]) is a GPU one.
pub fn auto_install(
    settings: &LlamaCppSettings,
    resolved: &Resolved,
    installed_ref: Option<&str>,
    hardware: &Hardware,
    tried: bool,
) -> AutoInstall {
    if resolved.source == SOURCE_CUSTOM || !settings.auto_install || tried {
        return AutoInstall::Keep;
    }
    if !resolved.server.is_file() {
        return AutoInstall::Missing;
    }
    if resolved.source == SOURCE_PACK
        && installed_ref.is_some_and(|release| ref_build(release) < pinned_build())
    {
        return AutoInstall::Outdated;
    }
    let wants_gpu = !pack_for(&settings.backend, hardware).ends_with("-cpu");
    let cpu_only = resolved
        .check
        .as_ref()
        .is_some_and(|check| check.gpu_visible == Some(false));
    if resolved.source == SOURCE_DETECTED && wants_gpu && cpu_only {
        AutoInstall::CpuOnly
    } else {
        AutoInstall::Keep
    }
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

/// What one archive may unpack to. An archive that goes past any limit is refused with
/// `InvalidData` and the staging folder holding the partial tree is removed.
///
/// llama.cpp's largest release (a CUDA build plus its runtime libraries) unpacks to a few GiB in
/// a few hundred files, so the real limits leave generous headroom while still stopping a
/// decompression bomb or an archive of millions of entries from filling the disk. Sizes are
/// counted from the bytes actually written or decompressed, never from sizes an entry declares.
/// The one declared figure used is a zip entry's compressed size, which bounds the ratio check
/// (the zip crate reads no more than that from the file). Sparse tar entries, whose holes would
/// expand without being counted, are refused.
#[derive(Clone, Copy)]
struct Limits {
    /// Total bytes unpacked from one archive (for a tar, decompressed bytes including headers).
    bytes: u64,
    /// Entries (files, folders and links) in one archive.
    entries: usize,
    /// A zip entry over this many bytes must not expand more than `ratio` times its compressed
    /// size.
    ratio_floor: u64,
    /// The largest uncompressed-to-compressed ratio allowed for a zip entry over `ratio_floor`.
    ratio: u64,
}

/// The limits for real installs: 8 GiB, 20 000 entries, and no entry over 100 MB that expands
/// more than 200:1.
const LIMITS: Limits = Limits {
    bytes: 8 << 30,
    entries: 20_000,
    ratio_floor: 100_000_000,
    ratio: 200,
};

fn over_limit(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn too_many_bytes(limits: Limits) -> std::io::Error {
    over_limit(format!(
        "archive unpacks to more than {} bytes",
        limits.bytes
    ))
}

/// Unpacks a `.zip` or `.tar.gz`; both refuse entries that would land outside `into` and
/// archives that go past [`LIMITS`].
fn extract(archive: &Path, into: &Path) -> Result<(), InstallError> {
    let context = || format!("unpacking {}", archive.display());
    let file = std::fs::File::open(archive).map_err(io(archive.display().to_string()))?;
    std::fs::create_dir_all(into).map_err(io(into.display().to_string()))?;
    if archive.extension().is_some_and(|ext| ext == "zip") {
        extract_zip(file, into, LIMITS).map_err(io(context()))
    } else {
        extract_tar(flate2::read::GzDecoder::new(file), into, LIMITS).map_err(io(context()))
    }
}

/// Fails with `InvalidData` once more than `left` bytes have been read through it, and records
/// that it did so in `tripped` so the cause survives whatever wraps the error.
struct CappedReader<R> {
    inner: R,
    left: u64,
    tripped: std::rc::Rc<std::cell::Cell<bool>>,
}

impl<R: std::io::Read> std::io::Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        match self.left.checked_sub(read as u64) {
            Some(left) => self.left = left,
            None => {
                self.tripped.set(true);
                return Err(over_limit("archive is over the byte limit".into()));
            }
        }
        Ok(read)
    }
}

/// Refuses tar entries other than regular files, folders and links that stay inside the archive:
/// sparse files, devices, FIFOs and unknown types (which the tar crate would write as regular
/// files), and symlinks or hard links whose target is absolute or contains `..`.
fn check_tar_entry<R: std::io::Read>(entry: &tar::Entry<'_, R>) -> std::io::Result<()> {
    use std::path::Component;
    let kind = entry.header().entry_type();
    let name = || String::from_utf8_lossy(&entry.path_bytes()).into_owned();
    let refuse = |why: &str| over_limit(format!("refusing tar entry {:?}: {why}", name()));
    if kind.is_gnu_sparse() {
        return Err(refuse("sparse files are not allowed"));
    }
    if kind.is_symlink() || kind.is_hard_link() {
        let target = entry
            .link_name()?
            .ok_or_else(|| refuse("link has no target"))?;
        if !target
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(refuse("link target is absolute or contains `..`"));
        }
        return Ok(());
    }
    if kind.is_file() || kind.is_dir() || kind.is_contiguous() {
        return Ok(());
    }
    Err(refuse("not a regular file, folder or link"))
}

/// Unpacks a tar stream into `into`. The tar crate's own path checks apply to each entry
/// (`unpack_in` skips an entry that would leave `into`, which is refused here), the entry count
/// is capped, and the decompressed stream is capped, so a small archive cannot expand without
/// bound. Long-name and pax headers are read whole by the tar crate, but only from that capped
/// stream. Entries are unpacked one at a time, so the crate's deferred handling of read-only
/// folders (applied after the whole archive) is lost; llama.cpp's folders are 0755.
fn extract_tar(reader: impl std::io::Read, into: &Path, limits: Limits) -> std::io::Result<()> {
    let tripped = std::rc::Rc::new(std::cell::Cell::new(false));
    let capped = CappedReader {
        inner: reader,
        left: limits.bytes,
        tripped: tripped.clone(),
    };
    let unpack = || -> std::io::Result<()> {
        let mut archive = tar::Archive::new(capped);
        for (count, entry) in archive.entries()?.enumerate() {
            if count >= limits.entries {
                return Err(over_limit(format!(
                    "archive holds more than {} entries",
                    limits.entries
                )));
            }
            let mut entry = entry?;
            check_tar_entry(&entry)?;
            if !entry.unpack_in(into)? {
                let name = entry.path_bytes().into_owned();
                return Err(over_limit(format!(
                    "refusing tar entry {:?}: path escapes the destination",
                    String::from_utf8_lossy(&name)
                )));
            }
        }
        Ok(())
    };
    unpack().map_err(|error| {
        if tripped.get() {
            too_many_bytes(limits)
        } else {
            error
        }
    })
}

/// Unpacks a zip into `into`, writing only regular files and folders beneath it.
///
/// Entries with an absolute path, a `..` component or a drive prefix are refused, as are symlinks
/// and any other non-regular entry (a FIFO or device entry, which the zip crate would otherwise
/// write as an ordinary file), so nothing can land or link outside `into`. Files are created
/// exclusively, so an entry can never be written through an existing link. The entry count and
/// the bytes actually written are held to `limits`.
fn extract_zip(file: std::fs::File, into: &Path, limits: Limits) -> std::io::Result<()> {
    use std::io::Read;
    use std::path::Component;
    let refuse = |name: &str, why: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("refusing zip entry {name:?}: {why}"),
        )
    };
    let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    if archive.len() > limits.entries {
        return Err(over_limit(format!(
            "archive holds more than {} entries",
            limits.entries
        )));
    }
    let mut written_total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(std::io::Error::other)?;
        let name = entry
            .name()
            .map_err(|_| refuse("?", "unreadable entry name"))?
            .into_owned();
        if name.split(['/', '\\']).any(|part| part == "..") {
            return Err(refuse(&name, "contains `..`"));
        }
        let bytes = name.as_bytes();
        if name.starts_with(['/', '\\']) || (bytes.len() > 1 && bytes[1] == b':') {
            return Err(refuse(&name, "path is absolute"));
        }
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| refuse(&name, "path escapes the destination"))?;
        if !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(refuse(&name, "path is absolute or has a prefix"));
        }
        // Only regular files and folders. `is_file` is just "not a folder and not a symlink", so
        // a FIFO or device entry has to be refused by its unix file type.
        if entry.is_symlink() {
            return Err(refuse(&name, "symlinks are not allowed"));
        }
        if let Some(kind) = entry.unix_mode().map(|mode| mode & 0o170_000) {
            let allowed = kind == 0 || kind == 0o100_000 || (kind == 0o040_000 && entry.is_dir());
            if !allowed {
                return Err(refuse(&name, "not a regular file or folder"));
            }
        }
        let target = into.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if !entry.is_file() {
            return Err(refuse(&name, "not a regular file"));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Read one byte past the allowance, so exceeding it is seen without writing much more.
        let remaining = limits.bytes - written_total;
        let ratio_cap = entry
            .compressed_size()
            .saturating_mul(limits.ratio)
            .max(limits.ratio_floor);
        let allowed = remaining.min(ratio_cap);
        let mut out = std::fs::File::create_new(&target)?;
        let written = std::io::copy(&mut (&mut entry).take(allowed + 1), &mut out)?;
        drop(out);
        if written > allowed {
            return Err(if allowed == remaining {
                too_many_bytes(limits)
            } else {
                refuse(&name, "expands too much for its compressed size")
            });
        }
        written_total += written;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// The folder holding `llama-server`, searched `depth` folders deep.
fn find_server_dir(dir: &Path, depth: usize) -> Option<PathBuf> {
    let exe = format!("llama-server{}", std::env::consts::EXE_SUFFIX);
    // A symlinked server or folder is not followed.
    if std::fs::symlink_metadata(dir.join(&exe)).is_ok_and(|meta| meta.is_file()) {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
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

/// [`verdict`], and a build whose `--help` lacks a flag Imperium passes (`missing`) is
/// broken too, so it is removed and the build in use before stays: the install rolls back.
pub fn install_verdict(
    pack: &str,
    check: &RuntimeCheck,
    missing: Option<&[&str]>,
    hardware: &Hardware,
) -> Verdict {
    match missing {
        Some(missing) if !missing.is_empty() && check.runs => Verdict::Broken(format!(
            "llama-server lacks flags Imperium passes ({})",
            missing.join(", ")
        )),
        _ => verdict(pack, check, hardware),
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
            let missing = if check.runs {
                runtime_check::missing_flags(&server).await
            } else {
                None
            };
            install_verdict(&pack, &check, missing.as_deref(), &hardware)
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
            vram_overridden: false,
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
        let machine = gpu_machine();
        let found = vec![
            (PathBuf::from("a"), checked(true, Some(false))),
            (PathBuf::from("b"), checked(true, None)),
            (PathBuf::from("c"), checked(true, Some(true))),
            (PathBuf::from("d"), checked(true, Some(true))),
        ];
        assert_eq!(best_found(&found, &machine), Some(2));
        assert_eq!(best_found(&found[..2], &machine), Some(1));
        assert_eq!(best_found(&found[..1], &machine), Some(0));
        assert_eq!(best_found(&[], &machine), None);
    }

    fn on(device: &str, version: Option<&str>) -> RuntimeCheck {
        RuntimeCheck {
            devices: vec![runtime_check::Device {
                id: device.into(),
                description: "GPU".into(),
            }],
            version: version.map(str::to_string),
            ..checked(true, Some(true))
        }
    }

    fn cpu_version(version: &str) -> RuntimeCheck {
        RuntimeCheck {
            version: Some(version.into()),
            ..checked(true, Some(false))
        }
    }

    #[test]
    fn a_build_on_the_gpu_backend_of_the_machine_wins_then_the_newest() {
        let machine = gpu_machine();
        let found = vec![
            (PathBuf::from("cpu"), checked(true, Some(false))),
            (
                PathBuf::from("vulkan"),
                on("Vulkan0", Some("version: 9000 (a)")),
            ),
            (
                PathBuf::from("old-cuda"),
                on("CUDA0", Some("version: 6000 (b)")),
            ),
            (
                PathBuf::from("new-cuda"),
                on("CUDA0", Some("version: 7000 (c)")),
            ),
        ];
        assert_eq!(
            best_found(&found, &machine),
            // macOS has no CUDA build, so any GPU ranks alike there and the newest wins.
            Some(if machine.backend_chain()[0].starts_with("cuda") {
                3
            } else {
                1
            }),
            "CUDA over Vulkan, newest first"
        );
        assert_eq!(
            best_found(&found[..2], &machine),
            Some(1),
            "any GPU over the CPU"
        );
        let mut no_gpu = machine.clone();
        no_gpu.gpus.clear();
        let cpu = vec![
            (PathBuf::from("old"), cpu_version("version: 5000 (x)")),
            (PathBuf::from("new"), cpu_version("version: b8000 (y)")),
        ];
        assert_eq!(best_found(&cpu, &no_gpu), Some(1));
    }

    #[test]
    fn build_numbers_are_read_from_versions_and_refs() {
        assert_eq!(build_number(Some("version: 6000 (58670d1)")), Some(6000));
        assert_eq!(build_number(Some("version: b12180 (x)")), Some(12180));
        assert_eq!(build_number(Some("built with MSVC")), None);
        assert_eq!(build_number(None), None);
        assert_eq!(ref_build("b12180"), Some(12180));
        assert_eq!(ref_build("master"), None);
        assert_eq!(pinned_build(), ref_build(llama_cpp_ref()));
    }

    /// A fake `llama-server` that records that it ran.
    #[cfg(unix)]
    fn marker_server(dir: &Path, marker: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let server = dir.join("llama-server");
        std::fs::write(
            &server,
            format!(
                "#!/bin/sh
echo ran >> '{}'
echo 'version: 6000 (abc)'
",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
        server
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unverified_download_is_listed_never_run_until_adopted() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let marker = home.path().join("ran.marker");
        let server = marker_server(&home.path().join("Downloads/llama-b99999"), &marker);
        let hardware = Hardware {
            gpus: Vec::new(),
            ram_total_bytes: 8 << 30,
            ram_available_bytes: 4 << 30,
            cpu_cores: 4,
            unified_memory: false,
            overridden: false,
            vram_overridden: false,
        };
        let listed = runtime_check::find_unverified(Some(home.path()));
        assert_eq!(listed, std::slice::from_ref(&server));
        let mut settings = llama_settings();
        let resolved = resolve(&settings, data.path(), &hardware, Some(home.path())).await;
        assert_ne!(resolved.server, server);
        assert!(!marker.exists(), "an unverified download was run");
        settings.custom_path = Some(server.clone());
        let adopted = resolve(&settings, data.path(), &hardware, Some(home.path())).await;
        assert_eq!(adopted.source, SOURCE_CUSTOM);
        assert_eq!(adopted.server, server);
        assert!(runtime_check::check(&adopted.server, &hardware).await.runs);
        assert!(marker.exists());
    }

    fn llama_settings() -> LlamaCppSettings {
        imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
            .settings()
            .unwrap()
            .runtime
            .llamacpp
    }

    fn resolved(source: &'static str, server: PathBuf, check: Option<RuntimeCheck>) -> Resolved {
        Resolved {
            source,
            pack: "windows-x64-cuda13".into(),
            server,
            check,
        }
    }

    #[test]
    fn auto_install_runs_when_nothing_usable_is_there_or_only_the_cpu_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("llama-server");
        std::fs::write(&present, "").unwrap();
        let missing = dir.path().join("none");
        let machine = gpu_machine();
        let settings = llama_settings();
        assert!(settings.auto_install, "on by default");
        let decide = |resolved: &Resolved, release: Option<&str>, tried: bool| {
            auto_install(&settings, resolved, release, &machine, tried)
        };
        let nothing = resolved(SOURCE_PACK, missing.clone(), None);
        assert_eq!(decide(&nothing, None, false), AutoInstall::Missing);
        assert_eq!(
            decide(&nothing, None, true),
            AutoInstall::Keep,
            "tried once"
        );
        let cpu_found = resolved(
            SOURCE_DETECTED,
            present.clone(),
            Some(checked(true, Some(false))),
        );
        // llama.cpp has no GPU build for an NVIDIA GPU on macOS.
        let gpu_build = !pack_for("auto", &machine).ends_with("-cpu");
        assert_eq!(
            decide(&cpu_found, None, false),
            if gpu_build {
                AutoInstall::CpuOnly
            } else {
                AutoInstall::Keep
            }
        );
        let gpu_found = resolved(
            SOURCE_DETECTED,
            present.clone(),
            Some(checked(true, Some(true))),
        );
        assert_eq!(decide(&gpu_found, None, false), AutoInstall::Keep);
        let custom = resolved(SOURCE_CUSTOM, missing.clone(), None);
        assert_eq!(
            decide(&custom, None, false),
            AutoInstall::Keep,
            "never over a custom path"
        );
        let installed = resolved(SOURCE_PACK, present.clone(), None);
        assert_eq!(
            decide(&installed, Some(llama_cpp_ref()), false),
            AutoInstall::Keep
        );
        assert_eq!(decide(&installed, Some("b1"), false), AutoInstall::Outdated);
        // A CPU build found on a machine without a GPU is what it should use.
        let mut no_gpu = machine.clone();
        no_gpu.gpus.clear();
        assert_eq!(
            auto_install(&settings, &cpu_found, None, &no_gpu, false),
            AutoInstall::Keep
        );
        let off = LlamaCppSettings {
            auto_install: false,
            ..settings.clone()
        };
        assert_eq!(
            auto_install(&off, &nothing, None, &machine, false),
            AutoInstall::Keep
        );
        let cpu_backend = LlamaCppSettings {
            backend: "cpu".into(),
            ..settings.clone()
        };
        assert_eq!(
            auto_install(&cpu_backend, &cpu_found, None, &machine, false),
            AutoInstall::Keep
        );
    }

    #[test]
    fn one_earlier_release_is_kept_until_the_pinned_one_has_loaded_a_model() {
        let installed: Vec<String> = ["b13000", "b12180", "b11000", "b10000", "b9000"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            refs_to_remove(&installed, "b12180", false),
            ["b10000", "b9000"]
        );
        assert_eq!(
            refs_to_remove(&installed, "b12180", true),
            ["b11000", "b10000", "b9000"]
        );
        assert!(refs_to_remove(&installed[..2], "b12180", true).is_empty());
    }

    #[test]
    fn an_earlier_release_is_used_until_the_pinned_one_is_installed() {
        let data = tempfile::tempdir().unwrap();
        let machine = gpu_machine();
        let packs = packs_for("auto", &machine);
        let old = releases_root(data.path())
            .join("b1")
            .join(archive_name(&packs[0]));
        let old_server = server_in(&old);
        std::fs::create_dir_all(old_server.parent().unwrap()).unwrap();
        std::fs::write(&old_server, "").unwrap();
        let (pack, server) = server_for(None, "auto", data.path(), &machine);
        assert_eq!((pack.as_str(), &server), (packs[0].as_str(), &old_server));
        assert_eq!(installed_ref(data.path(), &server).as_deref(), Some("b1"));
        let new_server = server_in(&pack_dir(data.path(), &packs[0]));
        std::fs::create_dir_all(new_server.parent().unwrap()).unwrap();
        std::fs::write(&new_server, "").unwrap();
        let (_, server) = server_for(None, "auto", data.path(), &machine);
        assert_eq!(server, new_server);
        remove_earlier_releases(data.path(), false);
        assert!(
            old_server.is_file(),
            "kept until the new build has loaded a model"
        );
        remove_earlier_releases(data.path(), true);
        assert!(!old_server.exists());
    }

    #[test]
    fn a_build_without_the_flags_imperium_passes_is_rolled_back() {
        let machine = gpu_machine();
        let good = checked(true, Some(true));
        let pack = "windows-x64-cuda13";
        assert_eq!(
            install_verdict(pack, &good, Some(&[]), &machine),
            Verdict::Good
        );
        let lacking = install_verdict(pack, &good, Some(&["--kv-unified-per-slot"]), &machine);
        assert!(
            matches!(&lacking, Verdict::Broken(why) if why.contains("--kv-unified-per-slot")),
            "{lacking:?}"
        );
        assert_eq!(
            install_verdict(pack, &good, None, &machine),
            Verdict::Good,
            "help unreadable: judged by the probe alone"
        );
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
        // Only the last build of the list (the CPU one) is installed: it is the one used.
        let last = packs.last().unwrap().clone();
        let installed = server_in(&pack_dir(data.path(), &last));
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "binary").unwrap();
        assert_eq!(
            server_for(None, "auto", data.path(), &machine),
            (last, installed)
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

    /// Writes `build`'s zip to a file under `root` and extracts it into `root/out`.
    fn extract_crafted(
        root: &Path,
        build: impl FnOnce(&mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>),
    ) -> std::io::Result<()> {
        extract_crafted_with(root, LIMITS, build)
    }

    fn extract_crafted_with(
        root: &Path,
        limits: Limits,
        build: impl FnOnce(&mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>),
    ) -> std::io::Result<()> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        build(&mut zip);
        extract_zip_bytes(root, zip.finish().unwrap().into_inner(), limits)
    }

    fn extract_zip_bytes(root: &Path, bytes: Vec<u8>, limits: Limits) -> std::io::Result<()> {
        let path = root.join("crafted.zip");
        std::fs::write(&path, bytes).unwrap();
        extract_zip(
            std::fs::File::open(&path).unwrap(),
            &root.join("out"),
            limits,
        )
    }

    /// Small limits so the caps can be hit with a few bytes.
    const SMALL: Limits = Limits {
        bytes: 1_000,
        entries: 2,
        ratio_floor: 100,
        ratio: 4,
    };

    fn add(zip: &mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>, name: &str) {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"payload").unwrap();
    }

    #[test]
    fn zip_extraction_unpacks_regular_entries() {
        let root = tempfile::tempdir().unwrap();
        extract_crafted(root.path(), |zip| {
            add(zip, "top/bin/llama-server");
            zip.add_directory("top/empty", zip::write::SimpleFileOptions::default())
                .unwrap();
        })
        .unwrap();
        let out = root.path().join("out");
        assert_eq!(
            std::fs::read(out.join("top/bin/llama-server")).unwrap(),
            b"payload"
        );
        assert!(out.join("top/empty").is_dir());
    }

    #[test]
    fn zip_extraction_refuses_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let error = extract_crafted(root.path(), |zip| add(zip, "../escaped.txt")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.path().join("escaped.txt").exists());
        let error =
            extract_crafted(root.path(), |zip| add(zip, "a/../../escaped.txt")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.path().join("escaped.txt").exists());
    }

    #[test]
    fn zip_extraction_refuses_absolute_paths() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("absolute.txt");
        let name = outside.to_string_lossy().replace('\\', "/");
        for entry in [name.as_str(), "/absolute.txt"] {
            let error = extract_crafted(root.path(), |zip| add(zip, entry)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{entry}");
        }
        assert!(!outside.exists());
    }

    #[test]
    fn zip_extraction_refuses_symlinks_pointing_outside() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let target = outside.to_string_lossy().into_owned();
        for link_target in ["../../outside", target.as_str()] {
            let error = extract_crafted(root.path(), |zip| {
                zip.add_symlink(
                    "link",
                    link_target,
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
                add(zip, "link/escaped.txt");
            })
            .unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidData,
                "{link_target}"
            );
            assert!(!root.path().join("out/link").exists());
            assert!(!outside.join("escaped.txt").exists());
        }
    }

    #[test]
    fn zip_extraction_refuses_backslash_traversal() {
        let root = tempfile::tempdir().unwrap();
        for entry in ["..\\escaped.txt", "a\\..\\..\\escaped.txt"] {
            let error = extract_crafted(root.path(), |zip| add(zip, entry)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{entry}");
            assert!(!root.path().join("escaped.txt").exists());
        }
    }

    #[test]
    fn zip_extraction_refuses_drive_relative_names() {
        let root = tempfile::tempdir().unwrap();
        for entry in ["C:x", "C:\\x", "c:/x"] {
            let error = extract_crafted(root.path(), |zip| add(zip, entry)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{entry}");
        }
        assert!(!root.path().join("out/x").exists());
    }

    #[test]
    fn zip_extraction_refuses_nul_in_names() {
        let root = tempfile::tempdir().unwrap();
        let error = extract_crafted(root.path(), |zip| add(zip, "bin/llama\0-server")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.path().join("out/bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn zip_extraction_keeps_the_exec_bit_of_regular_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        extract_crafted(root.path(), |zip| {
            zip.start_file(
                "llama-server",
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
            zip.write_all(b"payload").unwrap();
            zip.start_file(
                "plain.txt",
                zip::write::SimpleFileOptions::default().unix_permissions(0o644),
            )
            .unwrap();
            zip.write_all(b"payload").unwrap();
        })
        .unwrap();
        let mode = |name: &str| {
            std::fs::metadata(root.path().join("out").join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("llama-server"), 0o755);
        assert_eq!(mode("plain.txt"), 0o644);
    }

    /// Rewrites the unix file type of the only entry in `zip`'s bytes, as an archive made
    /// elsewhere could carry it.
    fn with_file_type(mut zip: Vec<u8>, file_type: u32) -> Vec<u8> {
        let central = zip
            .windows(4)
            .rposition(|window| window == b"PK\x01\x02")
            .unwrap();
        zip[central + 5] = 3; // made by: unix
        let mode = (file_type | 0o644) << 16;
        zip[central + 38..central + 42].copy_from_slice(&mode.to_le_bytes());
        zip
    }

    #[test]
    fn zip_extraction_refuses_non_regular_unix_entries() {
        let build = || {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            add(&mut zip, "pipe");
            zip.finish().unwrap().into_inner()
        };
        for (kind, file_type) in [
            ("fifo", 0o010_000),
            ("character device", 0o020_000),
            ("block device", 0o060_000),
            ("socket", 0o140_000),
            ("folder type on a file", 0o040_000),
        ] {
            let root = tempfile::tempdir().unwrap();
            let error = extract_zip_bytes(root.path(), with_file_type(build(), file_type), LIMITS)
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{kind}");
            assert!(!root.path().join("out/pipe").exists(), "{kind}");
        }
        let root = tempfile::tempdir().unwrap();
        extract_zip_bytes(root.path(), with_file_type(build(), 0o100_000), LIMITS).unwrap();
        assert!(root.path().join("out/pipe").is_file());
    }

    #[test]
    fn zip_extraction_caps_the_entry_count() {
        let root = tempfile::tempdir().unwrap();
        extract_crafted_with(root.path(), SMALL, |zip| {
            add(zip, "a");
            add(zip, "b");
        })
        .unwrap();
        let error = extract_crafted_with(root.path(), SMALL, |zip| {
            add(zip, "c");
            add(zip, "d");
            add(zip, "e");
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.path().join("out/e").exists());
    }

    #[test]
    fn zip_extraction_caps_the_bytes_written() {
        let root = tempfile::tempdir().unwrap();
        let limits = Limits {
            bytes: 10,
            entries: 100,
            ..SMALL
        };
        // Two 7-byte entries pass one at a time but not together.
        extract_crafted_with(root.path(), limits, |zip| add(zip, "a")).unwrap();
        let error = extract_crafted_with(root.path(), limits, |zip| {
            add(zip, "b");
            add(zip, "c");
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        // The entry that crossed the line is cut off, not written in full.
        assert!(
            root.path()
                .join("out/c")
                .metadata()
                .map_or(true, |meta| meta.len() <= 4)
        );
    }

    #[test]
    fn zip_extraction_caps_the_expansion_ratio() {
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let limits = Limits {
            bytes: 1 << 20,
            entries: 10,
            ..SMALL
        };
        // 10 000 zeros deflate to a few bytes: far past 4:1 and over the 100-byte floor.
        let root = tempfile::tempdir().unwrap();
        let error = extract_crafted_with(root.path(), limits, |zip| {
            zip.start_file("zeros", deflated).unwrap();
            zip.write_all(&[0; 10_000]).unwrap();
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        // Under the floor the ratio does not matter.
        let root = tempfile::tempdir().unwrap();
        extract_crafted_with(root.path(), limits, |zip| {
            zip.start_file("zeros", deflated).unwrap();
            zip.write_all(&[0; 90]).unwrap();
        })
        .unwrap();
    }

    fn untar(files: &[(&str, &[u8])], limits: Limits, root: &Path) -> std::io::Result<()> {
        let bytes = archive("crafted.tar.gz", files);
        std::fs::create_dir_all(root.join("out")).unwrap();
        extract_tar(
            flate2::read::GzDecoder::new(std::io::Cursor::new(bytes)),
            &root.join("out"),
            limits,
        )
    }

    #[test]
    fn tar_extraction_unpacks_regular_entries() {
        let root = tempfile::tempdir().unwrap();
        untar(&[("top/bin/llama-server", b"payload")], LIMITS, root.path()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("out/top/bin/llama-server")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn tar_extraction_caps_the_entry_count() {
        let root = tempfile::tempdir().unwrap();
        // Tar headers and padding alone are over 1 000 bytes per entry.
        let limits = Limits {
            bytes: 1 << 20,
            ..SMALL
        };
        untar(&[("a", b"x"), ("b", b"x")], limits, root.path()).unwrap();
        let error = untar(
            &[("c", b"x"), ("d", b"x"), ("e", b"x")],
            limits,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.path().join("out/e").exists());
    }

    #[test]
    fn tar_extraction_caps_the_decompressed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let limits = Limits {
            bytes: 4_000,
            entries: 100,
            ..SMALL
        };
        // 100 KB of zeros compress to almost nothing but unpack past the cap.
        let error = untar(&[("big", &[0; 100_000])], limits, root.path()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            root.path()
                .join("out/big")
                .metadata()
                .map_or(true, |meta| meta.len() < 100_000)
        );
    }

    /// A tar holding one header of `kind` named `name` (linking to `link`), with no data.
    fn tar_entry(kind: tar::EntryType, name: &str, link: &str) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_path(name).unwrap();
        header.set_size(0);
        header.set_mode(0o644);
        // Written to the header directly, as `set_link_name` is not meant for hostile targets.
        header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append(&header, std::io::empty()).unwrap();
        builder.into_inner().unwrap()
    }

    fn untar_bytes(bytes: Vec<u8>, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(root.join("out")).unwrap();
        extract_tar(std::io::Cursor::new(bytes), &root.join("out"), LIMITS)
    }

    #[test]
    fn tar_extraction_refuses_unsafe_entry_types() {
        use tar::EntryType;
        let cases = [
            ("sparse file", EntryType::GNUSparse, "x", ""),
            ("fifo", EntryType::Fifo, "x", ""),
            ("character device", EntryType::Char, "x", ""),
            ("block device", EntryType::Block, "x", ""),
            (
                "symlink to outside",
                EntryType::Symlink,
                "link",
                "../../outside",
            ),
            (
                "symlink to absolute path",
                EntryType::Symlink,
                "link",
                "/etc",
            ),
            (
                "hard link outside",
                EntryType::Link,
                "hard",
                "../../etc/passwd",
            ),
            (
                "hard link to absolute path",
                EntryType::Link,
                "hard",
                "/etc/passwd",
            ),
        ];
        for (what, kind, name, link) in cases {
            let root = tempfile::tempdir().unwrap();
            let error = untar_bytes(tar_entry(kind, name, link), root.path()).unwrap_err();
            // The tar crate may reject a bare sparse header itself, with its own error kind.
            if kind != EntryType::GNUSparse {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{what}");
            }
            assert!(
                std::fs::read_dir(root.path().join("out"))
                    .unwrap()
                    .next()
                    .is_none(),
                "{what}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tar_extraction_keeps_links_that_stay_inside() {
        use tar::EntryType;
        let root = tempfile::tempdir().unwrap();
        untar_bytes(
            tar_entry(EntryType::Symlink, "lib.so", "lib.so.1"),
            root.path(),
        )
        .unwrap();
        let link = root.path().join("out/lib.so");
        assert_eq!(std::fs::read_link(link).unwrap(), Path::new("lib.so.1"));
    }

    #[test]
    fn tar_extraction_refuses_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        // `append_data` rejects `..`, so the name goes into the header directly.
        header.as_old_mut().name[..13].copy_from_slice(b"../escaped.tx");
        header.set_cksum();
        builder.append(&header, &b"x"[..]).unwrap();
        let bytes = builder.into_inner().unwrap();
        std::fs::create_dir_all(root.path().join("out")).unwrap();
        let error = extract_tar(
            std::io::Cursor::new(bytes),
            &root.path().join("out"),
            LIMITS,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.path().join("escaped.tx").exists());
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
