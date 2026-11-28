//! Keeping llama.cpp's own build up to date. llama.cpp has no updater, so Imperium reads the
//! latest release of `ggml-org/llama.cpp` from GitHub (a check made at most once a day, kept in
//! `runtimes/llama.cpp/update-check.json`, and never downloading anything), plans which build
//! suits this machine and what it costs ([`plan`]), and decides where an update of a
//! `llama-server` the user keeps outside Imperium's folders goes: in a sibling folder, never
//! over the user's own ([`beside`]).
//!
//! Downloads are verified against the SHA-256 digests the release lists for each asset, from
//! addresses under `https://api.github.com/repos/ggml-org/llama.cpp/releases/assets/` only; an
//! asset without a digest is refused.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hardware::Hardware;
use crate::runtime::{
    InstallError, Release, expected_sha256, official_asset_prefix, packs_for, ref_build,
    releases_root, safe_file_name, select, valid_ref,
};

/// Where the builds come from, as the user is shown it.
pub const SOURCE: &str = "github.com/ggml-org/llama.cpp";
/// How long a check stays current: one a day at most.
pub const CHECK_INTERVAL_S: u64 = 24 * 60 * 60;
const CACHE_FILE: &str = "update-check.json";

/// What the last successful check read, kept between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checked {
    /// Unix seconds.
    pub checked_at: u64,
    pub release: Release,
}

fn cache_path(data_dir: &Path) -> PathBuf {
    releases_root(data_dir).join(CACHE_FILE)
}

/// The last check; `None` before the first, or when the file is unreadable or names a release
/// that is not a plain build.
pub fn read_cache(data_dir: &Path) -> Option<Checked> {
    let text = std::fs::read_to_string(cache_path(data_dir)).ok()?;
    let checked: Checked = serde_json::from_str(&text).ok()?;
    valid_ref(&checked.release.tag_name).then_some(checked)
}

/// Keeps `checked`.
pub fn write_cache(data_dir: &Path, checked: &Checked) -> std::io::Result<()> {
    let path = cache_path(data_dir);
    std::fs::create_dir_all(releases_root(data_dir))?;
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string(checked).map_err(std::io::Error::other)?;
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &path)
}

/// Seconds since the Unix epoch.
pub fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Whether the release should be read again at `now`: none was read, or the last was a day ago.
pub fn due(checked: Option<&Checked>, now: u64) -> bool {
    checked.is_none_or(|checked| now.saturating_sub(checked.checked_at) >= CHECK_INTERVAL_S)
}

/// Whether release `latest` is a newer build than `current` (the build number in use, from its
/// `--version` or its folder); `false` when either is unknown.
pub fn newer(latest: &str, current: Option<u64>) -> bool {
    matches!((ref_build(latest), current), (Some(latest), Some(current)) if latest > current)
}

/// What an update installs and costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The release, such as `b12900`.
    pub release: String,
    /// The build tried first, such as `windows-x64-cuda13`.
    pub pack: String,
    /// Bytes to download for that build (its archive and companions).
    pub bytes: u64,
    /// The files that would be downloaded.
    pub assets: Vec<String>,
}

/// The builds to try for an update, best first: those of [`packs_for`], without the CPU build
/// when this machine has a GPU (an update never trades a GPU build for the CPU's).
pub fn update_packs(backend: &str, hardware: &Hardware) -> Vec<String> {
    let mut packs = packs_for(backend, hardware);
    let has_gpu = !hardware.gpus.is_empty() || hardware.unified_memory;
    if has_gpu && packs.iter().any(|pack| !pack.ends_with("-cpu")) {
        packs.retain(|pack| !pack.ends_with("-cpu"));
    }
    packs
}

/// What installing `release` for the first of `packs` it has would download, after checking
/// every build it could try: each asset must have a plain file name, an official address under
/// `api` (normally [`crate::runtime::GITHUB_API`]) and a SHA-256 digest.
pub fn plan(release: &Release, packs: &[String], api: &str) -> Result<Plan, InstallError> {
    if !valid_ref(&release.tag_name) {
        return Err(InstallError::Release(
            release.tag_name.clone(),
            "the release is not a build such as b12180".into(),
        ));
    }
    let prefix = official_asset_prefix(api);
    let mut first: Option<Plan> = None;
    for pack in packs {
        let Some(selection) = select(release, pack) else {
            continue;
        };
        for asset in &selection.assets {
            let id = asset.url.strip_prefix(&prefix).unwrap_or_default();
            if !safe_file_name(&asset.name)
                || id.is_empty()
                || !id.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(InstallError::Download(
                    asset.name.clone(),
                    format!("{} is not an official llama.cpp release asset", asset.url),
                ));
            }
            expected_sha256(asset)?;
        }
        if first.is_none() {
            first = Some(Plan {
                release: release.tag_name.clone(),
                pack: pack.clone(),
                bytes: selection.total_bytes(),
                assets: selection
                    .assets
                    .iter()
                    .map(|asset| asset.name.clone())
                    .collect(),
            });
        }
    }
    first.ok_or_else(|| InstallError::NoBuild {
        release: release.tag_name.clone(),
        wanted: packs.join(", "),
    })
}

/// The folder an update of the `llama-server` at `server` goes in: the folder that holds its
/// release's folder (`D:/AI-Models/llama.cpp` for `D:/AI-Models/llama.cpp/b12823/bin/llama-server`).
/// Only for a `llama-server` in a folder named like a llama.cpp build (`b12823`, `llama-...`)
/// that has a parent folder of its own.
pub fn beside_container(server: &Path) -> Result<PathBuf, String> {
    let folder = server
        .parent()
        .ok_or_else(|| format!("{} has no folder", server.display()))?;
    let build = if folder
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        folder.parent().unwrap_or(folder)
    } else {
        folder
    };
    let name = build
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ref_build(&name).is_none() && !name.starts_with("llama") {
        return Err(format!(
            "{} is not in a llama.cpp folder (one named like b12823 or llama-...), so there is no \
             sibling folder to put a newer build in",
            server.display()
        ));
    }
    build
        .parent()
        .filter(|container| container.file_name().is_some())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "{} has no folder around its llama.cpp folder to put a newer build beside it",
                server.display()
            )
        })
}

/// The new folder for `release` next to the llama.cpp folder of `server`
/// (`D:/AI-Models/llama.cpp/b12900` for the example of [`beside_container`]).
pub fn beside(server: &Path, release: &str) -> Result<PathBuf, String> {
    if !valid_ref(release) {
        return Err(format!("{release} is not a llama.cpp build such as b12900"));
    }
    beside_container(server).map(|container| container.join(release))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::io::Write;
    use std::pin::Pin;

    use axum::Router;
    use axum::routing::get;
    use imperium_api::encoding::hex;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::runtime::{
        Place, Verdict, activate, fetch_latest, install_first_working, install_first_working_at,
        installed_refs, pack_dir_in, read_active, server_path, use_build,
    };

    fn exe() -> String {
        format!("llama-server{}", std::env::consts::EXE_SUFFIX)
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

    /// A GitHub-like server for one release `tag`: its listing at the `latest` path and the
    /// assets at the addresses the official API uses. `files` are (name, bytes, digest); a
    /// digest of `None` is the real one. Returns the server's address and the release.
    async fn serve(tag: &str, files: Vec<(String, Vec<u8>, Option<String>)>) -> (String, Release) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut app = Router::new();
        let mut assets = Vec::new();
        for (id, (name, body, digest)) in files.into_iter().enumerate() {
            let address = format!("{url}/repos/ggml-org/llama.cpp/releases/assets/{}", id + 1);
            assets.push(serde_json::json!({
                "name": name,
                "url": address,
                "size": body.len(),
                "digest": digest.unwrap_or_else(|| format!("sha256:{}", hex(&Sha256::digest(&body)))),
            }));
            app = app.route(
                &format!("/repos/ggml-org/llama.cpp/releases/assets/{}", id + 1),
                get(move || async move { body }),
            );
        }
        let listing = serde_json::json!({ "tag_name": tag, "assets": assets });
        let text = listing.to_string();
        app = app.route(
            "/repos/ggml-org/llama.cpp/releases/latest",
            get(move || async move { text }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await });
        (url, serde_json::from_value(listing).unwrap())
    }

    fn accept_all(_: String, _: PathBuf) -> Pin<Box<dyn Future<Output = Verdict> + Send>> {
        Box::pin(async { Verdict::Good })
    }

    fn cpu_zip(tag: &str) -> String {
        format!("llama-{tag}-bin-win-cpu-x64.zip")
    }

    /// A server for `tag`'s Windows CPU build whose `llama-server` holds `marker`.
    async fn serve_cpu(tag: &str, marker: &str) -> (String, Release) {
        let name = cpu_zip(tag);
        let exe = exe();
        let body = archive(&name, &[(&exe, marker.as_bytes())]);
        serve(tag, vec![(name, body, None)]).await
    }

    fn cpu() -> Vec<String> {
        vec!["windows-x64-cpu".to_string()]
    }

    #[tokio::test]
    async fn the_latest_release_is_read_and_planned_for_this_machine() {
        let cuda = "llama-b12900-bin-win-cuda-13.5-x64.zip";
        let cudart = "cudart-llama-bin-win-cuda-13.5-x64.zip";
        let vulkan = "llama-b12900-bin-win-vulkan-x64.zip";
        let (url, _) = serve(
            "b12900",
            vec![
                (cuda.into(), vec![1; 300], None),
                (cudart.into(), vec![2; 100], None),
                (vulkan.into(), vec![3; 50], None),
                ("llama-b12900-ui.tar.gz".into(), vec![4; 10], None),
            ],
        )
        .await;
        let release = fetch_latest(&reqwest::Client::new(), &url).await.unwrap();
        assert_eq!(release.tag_name, "b12900");
        assert_eq!(release.assets.len(), 4);
        let packs = [
            "windows-x64-cuda13",
            "windows-x64-vulkan",
            "windows-x64-cpu",
        ]
        .map(String::from)
        .to_vec();
        let plan = plan(&release, &packs, &url).unwrap();
        assert_eq!(plan.release, "b12900");
        assert_eq!(plan.pack, "windows-x64-cuda13");
        assert_eq!(plan.bytes, 400, "the CUDA build and its runtime libraries");
        assert_eq!(plan.assets, [cuda, cudart]);
        // Without the CUDA build in the list, the first pack that has a build is the plan.
        let plan = super::plan(&release, &packs[1..], &url).unwrap();
        assert_eq!(plan.pack, "windows-x64-vulkan");
        let none = ["windows-x64-rocm".to_string()];
        assert!(matches!(
            super::plan(&release, &none, &url),
            Err(InstallError::NoBuild { .. })
        ));
    }

    #[tokio::test]
    async fn a_release_that_is_not_a_plain_build_is_refused() {
        let (url, _) = serve("../../evil", vec![]).await;
        let error = fetch_latest(&reqwest::Client::new(), &url)
            .await
            .unwrap_err();
        assert!(matches!(error, InstallError::Release(..)), "{error}");
        for tag in ["b+5", "b", "12180", "b12180/x", ""] {
            assert!(!valid_ref(tag), "{tag}");
        }
        assert!(valid_ref("b12900"));
    }

    #[tokio::test]
    async fn assets_without_a_digest_or_from_elsewhere_are_not_planned() {
        let (url, mut release) = serve_cpu("b12900", "x").await;
        assert!(plan(&release, &cpu(), &url).is_ok());
        let mut unsigned = release.clone();
        unsigned.assets[0].digest = None;
        assert!(matches!(
            plan(&unsigned, &cpu(), &url),
            Err(InstallError::NoDigest(_))
        ));
        let mut weak = release.clone();
        weak.assets[0].digest = Some(format!("sha1:{}", "a".repeat(40)));
        assert!(matches!(
            plan(&weak, &cpu(), &url),
            Err(InstallError::NoDigest(_))
        ));
        // Only the official API's asset addresses are used, never a link a release names.
        release.assets[0].url = "https://evil.example/llama.zip".into();
        assert!(matches!(
            plan(&release, &cpu(), &url),
            Err(InstallError::Download(..))
        ));
        release.assets[0].url = format!("{url}/repos/other/repo/releases/assets/1");
        assert!(matches!(
            plan(&release, &cpu(), &url),
            Err(InstallError::Download(..))
        ));
        // And a file name is a plain name.
        let (url, mut release) = serve_cpu("b12900", "x").await;
        release.assets[0].name = "../llama-b12900-bin-win-cpu-x64.zip".into();
        assert!(plan(&release, &cpu(), &url).is_err());
    }

    #[test]
    fn checks_are_made_at_most_once_a_day_and_cached() {
        let data = tempfile::tempdir().unwrap();
        assert!(read_cache(data.path()).is_none());
        assert!(due(None, 1_000));
        let checked = Checked {
            checked_at: 1_000,
            release: serde_json::from_value(serde_json::json!({
                "tag_name": "b12900", "assets": []
            }))
            .unwrap(),
        };
        write_cache(data.path(), &checked).unwrap();
        let read = read_cache(data.path()).unwrap();
        assert_eq!(read.checked_at, 1_000);
        assert_eq!(read.release.tag_name, "b12900");
        assert!(!due(Some(&read), 1_000 + CHECK_INTERVAL_S - 1));
        assert!(due(Some(&read), 1_000 + CHECK_INTERVAL_S));
        // A clock that went back does not make a check due.
        assert!(!due(Some(&read), 10));
        // A cache naming something that is not a build is not trusted.
        std::fs::write(
            cache_path(data.path()),
            r#"{"checked_at":1,"release":{"tag_name":"../x","assets":[]}}"#,
        )
        .unwrap();
        assert!(read_cache(data.path()).is_none());
    }

    #[test]
    fn newer_builds_are_told_from_older_ones_and_unknown_ones() {
        assert!(newer("b12900", Some(12823)));
        assert!(!newer("b12823", Some(12823)));
        assert!(!newer("b12000", Some(12823)));
        assert!(!newer("b12900", None));
        assert!(!newer("latest", Some(1)));
    }

    #[test]
    fn a_gpu_machine_updates_to_a_gpu_build_only() {
        let machine = |gpus| Hardware {
            gpus,
            ram_total_bytes: 16 << 30,
            ram_available_bytes: 8 << 30,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
            vram_overridden: false,
        };
        if cfg!(target_os = "macos") {
            // Apple builds are Metal or CPU, chosen by the memory model, not by a GPU list.
            return;
        }
        let none = update_packs("cpu", &machine(Vec::new()));
        assert!(none.iter().all(|pack| pack.ends_with("-cpu")), "{none:?}");
        let gpu = update_packs(
            "auto",
            &machine(vec![crate::hardware::Gpu {
                name: "RTX".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: 12 << 30,
                vram_free_bytes: 12 << 30,
                driver: Some("581.57".into()),
            }]),
        );
        assert!(!gpu.is_empty());
        assert!(gpu.iter().all(|pack| !pack.ends_with("-cpu")), "{gpu:?}");
    }

    #[test]
    fn a_sibling_folder_is_found_beside_the_users_llama_cpp_folder() {
        let new = |path: &str| {
            beside(Path::new(path), "b12900")
                .map(|folder| folder.to_string_lossy().replace('\\', "/"))
        };
        assert_eq!(
            new("D:/AI-Models/llama.cpp/b12823/bin/llama-server.exe").unwrap(),
            "D:/AI-Models/llama.cpp/b12900"
        );
        assert_eq!(
            new("/opt/llama/b12823/llama-server").unwrap(),
            "/opt/llama/b12900"
        );
        assert_eq!(
            new("/home/me/Downloads/llama-b9000-bin-ubuntu-x64/llama-server").unwrap(),
            "/home/me/Downloads/b12900"
        );
        // Not a llama.cpp folder: nothing is guessed (a system folder is never a container).
        assert!(new("/usr/bin/llama-server").is_err());
        assert!(new("/usr/local/bin/llama-server").is_err());
        assert!(new("llama-server").is_err());
        assert!(beside(Path::new("/opt/llama/b1/llama-server"), "../x").is_err());
    }

    /// Installs `release` under `data` as an earlier build did, and switches to it.
    async fn install_managed(data: &Path, tag: &str, marker: &str) -> PathBuf {
        let (_, release) = serve_cpu(tag, marker).await;
        let installed = install_first_working(
            &reqwest::Client::new(),
            &release,
            data,
            &cpu(),
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap();
        assert_eq!(installed.release, tag);
        activate(data, tag).unwrap();
        installed.server
    }

    fn hardware() -> Hardware {
        Hardware {
            gpus: Vec::new(),
            ram_total_bytes: 16 << 30,
            ram_available_bytes: 8 << 30,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
            vram_overridden: false,
        }
    }

    #[tokio::test]
    async fn an_update_installs_beside_the_old_build_and_can_be_rolled_back() {
        let data = tempfile::tempdir().unwrap();
        let old = install_managed(data.path(), "b12180", "old").await;
        let machine = hardware();
        // The update is a newer release, installed next to the first and switched to.
        let new = install_managed(data.path(), "b12900", "new").await;
        assert_ne!(old, new);
        assert_eq!(
            std::fs::read(&old).unwrap(),
            b"old",
            "the old build is untouched"
        );
        assert_eq!(std::fs::read(&new).unwrap(), b"new");
        assert_eq!(installed_refs(data.path()), ["b12900", "b12180"]);
        let active = read_active(data.path()).unwrap();
        assert_eq!(active.release, "b12900");
        assert_eq!(active.previous.as_deref(), Some("b12180"));
        // New loads use the new build, though the pinned one is older.
        let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
            .settings()
            .unwrap();
        let used = |data: &Path| server_path(&settings.runtime.llamacpp, data, &machine).1;
        let fake = |release: &str| {
            // The pack the machine's chain names, in the release's folder.
            let pack = packs_for("auto", &machine)[0].clone();
            let dir = pack_dir_in(data.path(), release, &pack);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(crate::runtime::server_in(&dir), release).unwrap();
            crate::runtime::server_in(&dir)
        };
        let (new_server, old_server) = (fake("b12900"), fake("b12180"));
        assert_eq!(used(data.path()), new_server);
        // Rolling back switches to the old one and keeps the new one for going forward.
        use_build(data.path(), "b12180").unwrap();
        assert_eq!(used(data.path()), old_server);
        assert_eq!(
            read_active(data.path()).unwrap().previous.as_deref(),
            Some("b12900")
        );
        assert_eq!(installed_refs(data.path()), ["b12900", "b12180"]);
        // A build that is not installed, or not a build, is refused and nothing changes.
        assert!(use_build(data.path(), "b11111").is_err());
        assert!(use_build(data.path(), "../b12180").is_err());
        assert_eq!(read_active(data.path()).unwrap().release, "b12180");
    }

    #[tokio::test]
    async fn at_most_two_builds_are_kept() {
        let data = tempfile::tempdir().unwrap();
        install_managed(data.path(), "b12180", "a").await;
        install_managed(data.path(), "b12500", "b").await;
        install_managed(data.path(), "b12900", "c").await;
        assert_eq!(installed_refs(data.path()), ["b12900", "b12500"]);
        let active = read_active(data.path()).unwrap();
        assert_eq!(
            (active.release.as_str(), active.previous.as_deref()),
            ("b12900", Some("b12500"))
        );
        // Activating the same build again keeps the one kept before it.
        activate(data.path(), "b12900").unwrap();
        assert_eq!(
            read_active(data.path()).unwrap().previous.as_deref(),
            Some("b12500")
        );
    }

    #[tokio::test]
    async fn a_download_that_does_not_match_its_digest_leaves_the_builds_alone() {
        let data = tempfile::tempdir().unwrap();
        let old = install_managed(data.path(), "b12180", "old").await;
        let name = cpu_zip("b12900");
        let exe = exe();
        let (_, tampered) = serve(
            "b12900",
            vec![(
                name,
                archive(&cpu_zip("b12900"), &[(&exe, b"evil")]),
                Some(format!("sha256:{}", "0".repeat(64))),
            )],
        )
        .await;
        let error = install_first_working(
            &reqwest::Client::new(),
            &tampered,
            data.path(),
            &cpu(),
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::Checksum { .. }), "{error}");
        assert_eq!(installed_refs(data.path()), ["b12180"]);
        assert_eq!(read_active(data.path()).unwrap().release, "b12180");
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
        assert!(!pack_dir_in(data.path(), "b12900", "windows-x64-cpu").exists());
        // The staging folder is gone too.
        assert!(!releases_root(data.path()).join("b12900").exists());
    }

    #[tokio::test]
    async fn a_build_that_fails_its_smoke_test_is_not_switched_to() {
        let data = tempfile::tempdir().unwrap();
        install_managed(data.path(), "b12180", "old").await;
        let (_, release) = serve_cpu("b12900", "new").await;
        let broken = |_: String, _: PathBuf| {
            Box::pin(async { Verdict::Broken("does not start".into()) })
                as Pin<Box<dyn Future<Output = Verdict> + Send>>
        };
        let error = install_first_working(
            &reqwest::Client::new(),
            &release,
            data.path(),
            &cpu(),
            &broken,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::NothingWorks(_)), "{error}");
        assert_eq!(installed_refs(data.path()), ["b12180"]);
        assert_eq!(read_active(data.path()).unwrap().release, "b12180");
    }

    #[tokio::test]
    async fn updating_a_folder_of_the_users_installs_beside_it_and_never_touches_it() {
        let root = tempfile::tempdir().unwrap();
        let container = root.path().join("llama.cpp");
        let old_bin = container.join("b12823").join("bin");
        std::fs::create_dir_all(&old_bin).unwrap();
        let old_server = old_bin.join(exe());
        std::fs::write(&old_server, b"mine").unwrap();
        std::fs::write(old_bin.join("ggml-cuda.dll"), b"my dll").unwrap();
        let (_, release) = serve_cpu("b12900", "new").await;
        let http = reqwest::Client::new();

        assert_eq!(beside_container(&old_server).unwrap(), container);
        let place = Place::Beside(beside_container(&old_server).unwrap());
        let installed =
            install_first_working_at(&http, &release, &place, &cpu(), &accept_all, &|_, _, _| {})
                .await
                .unwrap();
        let new_server = container.join("b12900").join("bin").join(exe());
        assert_eq!(installed.server, new_server);
        assert_eq!(std::fs::read(&new_server).unwrap(), b"new");
        assert_eq!(std::fs::read(&old_server).unwrap(), b"mine");
        assert_eq!(
            std::fs::read(old_bin.join("ggml-cuda.dll")).unwrap(),
            b"my dll"
        );
        let mut names: Vec<String> = std::fs::read_dir(&container)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["b12823", "b12900"],
            "no staging folder is left behind"
        );

        // Doing it again never replaces the folder that is there.
        let error =
            install_first_working_at(&http, &release, &place, &cpu(), &accept_all, &|_, _, _| {})
                .await
                .unwrap_err();
        assert!(matches!(error, InstallError::Io(..)), "{error}");
        assert_eq!(std::fs::read(&new_server).unwrap(), b"new");
        assert_eq!(std::fs::read(&old_server).unwrap(), b"mine");
    }

    #[tokio::test]
    async fn a_failed_update_of_the_users_folder_leaves_only_what_was_there() {
        let root = tempfile::tempdir().unwrap();
        let container = root.path().join("llama.cpp");
        let old_bin = container.join("b12823").join("bin");
        std::fs::create_dir_all(&old_bin).unwrap();
        std::fs::write(old_bin.join(exe()), b"mine").unwrap();
        let (_, release) = serve_cpu("b12900", "new").await;
        let broken = |_: String, _: PathBuf| {
            Box::pin(async { Verdict::Broken("does not start".into()) })
                as Pin<Box<dyn Future<Output = Verdict> + Send>>
        };
        let error = install_first_working_at(
            &reqwest::Client::new(),
            &release,
            &Place::Beside(container.clone()),
            &cpu(),
            &broken,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::NothingWorks(_)), "{error}");
        let names: Vec<_> = std::fs::read_dir(&container)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["b12823"]);
        assert_eq!(std::fs::read(old_bin.join(exe())).unwrap(), b"mine");
    }

    #[tokio::test]
    async fn an_archive_that_escapes_its_folder_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let container = root.path().join("llama.cpp");
        std::fs::create_dir_all(container.join("b12823")).unwrap();
        let name = cpu_zip("b12900");
        let exe = exe();
        // A zip whose entry climbs out of the folder it is unpacked in.
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for path in [exe.as_str(), "../../escaped.txt"] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"payload").unwrap();
        }
        let body = zip.finish().unwrap().into_inner();
        let (_, release) = serve("b12900", vec![(name, body, None)]).await;
        let error = install_first_working_at(
            &reqwest::Client::new(),
            &release,
            &Place::Beside(container.clone()),
            &cpu(),
            &accept_all,
            &|_, _, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InstallError::Io(..)), "{error}");
        assert!(!root.path().join("escaped.txt").exists());
        assert!(!container.join("escaped.txt").exists());
        assert!(!container.join("b12900").exists());
        let names: Vec<_> = std::fs::read_dir(&container)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["b12823"]);
    }
}
