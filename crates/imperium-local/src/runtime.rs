//! llama.cpp runtime packs: where they are installed, finding `llama-server`, and installing a
//! pack from the release CI publishes, verified against its `SHA256SUMS`.

use std::io::Write;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use imperium_config::LlamaCppSettings;
use sha2::{Digest, Sha256};

use crate::hardware::{Hardware, platform};

/// Where CI publishes runtime packs.
pub const RELEASES_URL: &str = "https://github.com/Densetsu152637/Imperium-AI/releases/download";

/// The llama.cpp release the packs are built from, pinned in `runtimes/llama-cpp.env`.
pub fn llama_cpp_ref() -> &'static str {
    include_str!("../../../runtimes/llama-cpp.env")
        .lines()
        .find_map(|line| line.strip_prefix("LLAMA_CPP_REF="))
        .map(str::trim)
        .expect("runtimes/llama-cpp.env sets LLAMA_CPP_REF")
}

/// A pack's archive name, as CI names it.
pub fn archive_name(pack: &str) -> String {
    format!("imperium-runtime-llama.cpp-{pack}")
}

/// The folder a pack is installed in.
pub fn pack_dir(data_dir: &Path, pack: &str) -> PathBuf {
    data_dir
        .join("runtimes")
        .join("llama.cpp")
        .join(llama_cpp_ref())
        .join(archive_name(pack))
}

/// `llama-server` inside an installed pack.
pub fn server_in(pack_dir: &Path) -> PathBuf {
    pack_dir
        .join("bin")
        .join(format!("llama-server{}", std::env::consts::EXE_SUFFIX))
}

/// The runtime pack to use: the configured backend on this platform, or for `auto` the one
/// that suits `hardware`.
pub fn pack_for(backend: &str, hardware: &Hardware) -> String {
    match backend {
        "auto" | "" => hardware.suggested_pack(),
        backend => format!("{}-{backend}", platform()),
    }
}

/// The `llama-server` to run and the pack it comes from (`custom` for `custom_path`).
pub fn server_path(
    settings: &LlamaCppSettings,
    data_dir: &Path,
    hardware: &Hardware,
) -> (String, PathBuf) {
    match &settings.custom_path {
        Some(path) => ("custom".into(), path.clone()),
        None => {
            let pack = pack_for(&settings.backend, hardware);
            let path = server_in(&pack_dir(data_dir, &pack));
            (pack, path)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("download of {0} failed: {1}")]
    Download(String, String),
    #[error("SHA256SUMS has no entry for {0}")]
    NotListed(String),
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("the pack does not contain {0}")]
    MissingServer(String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> InstallError {
    let context = context.into();
    move |error| InstallError::Io(context, error)
}

/// Downloads `pack` from `base_url/runtime-llama.cpp-<ref>/`, checks it against the release's
/// `SHA256SUMS`, and unpacks it under `data_dir`. Returns the path of `llama-server`.
pub async fn install(
    http: &reqwest::Client,
    base_url: &str,
    data_dir: &Path,
    pack: &str,
) -> Result<PathBuf, InstallError> {
    let release = format!("{base_url}/runtime-llama.cpp-{}", llama_cpp_ref());
    let archive = format!("{}.tar.gz", archive_name(pack));
    let sums = get(http, &format!("{release}/SHA256SUMS"))
        .await?
        .text()
        .await
        .map_err(|error| InstallError::Download("SHA256SUMS".into(), error.to_string()))?;
    let expected = sums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| name.trim().trim_start_matches('*') == archive)
        .map(|(sum, _)| sum.to_ascii_lowercase())
        .ok_or_else(|| InstallError::NotListed(archive.clone()))?;

    let root = data_dir
        .join("runtimes")
        .join("llama.cpp")
        .join(llama_cpp_ref());
    std::fs::create_dir_all(&root).map_err(io(root.display().to_string()))?;
    let download = root.join(format!("{archive}.part"));
    let mut file = std::fs::File::create(&download).map_err(io(download.display().to_string()))?;
    let mut hasher = Sha256::new();
    let mut body = get(http, &format!("{release}/{archive}"))
        .await?
        .bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk =
            chunk.map_err(|error| InstallError::Download(archive.clone(), error.to_string()))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(io(download.display().to_string()))?;
    }
    drop(file);
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(&download);
        return Err(InstallError::Checksum {
            file: archive,
            expected,
            actual,
        });
    }
    let unpacked = unpack(&download, &root, pack);
    let _ = std::fs::remove_file(&download);
    unpacked
}

async fn get(http: &reqwest::Client, url: &str) -> Result<reqwest::Response, InstallError> {
    http.get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| InstallError::Download(url.to_string(), error.to_string()))
}

/// Unpacks into a staging folder, then moves the pack into place so a failed unpack never
/// leaves a half-installed pack where the daemon looks.
fn unpack(archive: &Path, root: &Path, pack: &str) -> Result<PathBuf, InstallError> {
    let staging = root.join(format!(".staging-{pack}"));
    let _ = std::fs::remove_dir_all(&staging);
    let file = std::fs::File::open(archive).map_err(io(archive.display().to_string()))?;
    // `unpack` refuses entries that would land outside the staging folder.
    tar::Archive::new(flate2::read::GzDecoder::new(file))
        .unpack(&staging)
        .map_err(io(format!("unpacking {}", archive.display())))?;
    let name = archive_name(pack);
    let server = server_in(&staging.join(&name));
    if !server.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(InstallError::MissingServer(format!(
            "{name}/bin/llama-server"
        )));
    }
    let target = root.join(&name);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(staging.join(&name), &target).map_err(io(target.display().to_string()))?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(server_in(&target))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get as get_route;

    fn pack_archive(pack: &str, with_server: bool) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let name = archive_name(pack);
        let mut add = |path: String, contents: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, contents).unwrap();
        };
        add(format!("{name}/manifest.json"), b"{}");
        if with_server {
            let exe = format!("llama-server{}", std::env::consts::EXE_SUFFIX);
            add(format!("{name}/bin/{exe}"), b"binary");
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    async fn serve(files: Vec<(String, Vec<u8>)>) -> String {
        let mut app = Router::new();
        for (name, body) in files {
            let path = format!("/runtime-llama.cpp-{}/{name}", llama_cpp_ref());
            app = app.route(&path, get_route(move || async move { body }));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        url
    }

    #[test]
    fn the_pinned_ref_is_read_from_the_env_file() {
        assert!(llama_cpp_ref().starts_with('b'), "{}", llama_cpp_ref());
    }

    #[tokio::test]
    async fn installs_verified_packs_and_rejects_tampered_ones() {
        let pack = "test-cpu";
        let archive = pack_archive(pack, true);
        let file = format!("{}.tar.gz", archive_name(pack));
        let sums = format!("{}  {file}\n", hex(&Sha256::digest(&archive)));
        let url = serve(vec![
            ("SHA256SUMS".into(), sums.into_bytes()),
            (file.clone(), archive),
        ])
        .await;
        let data = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let server = install(&http, &url, data.path(), pack).await.unwrap();
        assert!(server.is_file());
        assert_eq!(server, server_in(&pack_dir(data.path(), pack)));

        let tampered = serve(vec![
            (
                "SHA256SUMS".into(),
                format!("{}  {file}\n", "0".repeat(64)).into_bytes(),
            ),
            (file, pack_archive(pack, true)),
        ])
        .await;
        let error = install(&http, &tampered, data.path(), pack)
            .await
            .unwrap_err();
        assert!(matches!(error, InstallError::Checksum { .. }), "{error}");
        // The earlier install is untouched.
        assert!(server.is_file());
    }

    #[tokio::test]
    async fn packs_without_a_server_are_refused() {
        let pack = "empty-cpu";
        let archive = pack_archive(pack, false);
        let file = format!("{}.tar.gz", archive_name(pack));
        let sums = format!("{}  {file}\n", hex(&Sha256::digest(&archive)));
        let url = serve(vec![
            ("SHA256SUMS".into(), sums.into_bytes()),
            (file, archive),
        ])
        .await;
        let data = tempfile::tempdir().unwrap();
        let error = install(&reqwest::Client::new(), &url, data.path(), pack)
            .await
            .unwrap_err();
        assert!(matches!(error, InstallError::MissingServer(_)), "{error}");
        assert!(!pack_dir(data.path(), pack).exists());
    }
}
