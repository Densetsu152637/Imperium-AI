//! Saving a file a page offered (`browser_download`): into one folder chosen by the session (never
//! by the page), under a name made safe, within a size cap, and never executed or marked
//! executable. The request goes through the same redirect rules and `robots.txt` check as
//! `web_fetch`, on a client with its own timeouts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use url::Url;

use crate::{Web, WebError};

/// Longest file name kept, in characters.
const MAX_NAME: usize = 100;
/// The longest one download may take in all (each wait for data has its own, shorter limit).
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(600);

/// Extensions of files that do nothing when a person opens them: documents, images, media,
/// archives and plain data. Any other name, however harmless it looks (`.scf`, `.url`, `.msc`,
/// `.iso`, `.docm`, `.xlsm` and `.library-ms` among thousands), is saved with `.download` added so
/// nothing runs, mounts or loads it by accident; its content is untouched. A name with no
/// extension cannot be launched and is kept as it is.
const INERT: &[&str] = &[
    "txt", "md", "csv", "tsv", "json", "jsonl", "xml", "yaml", "yml", "toml", "log", "ini", "pdf",
    "rtf", "epub", "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "heic",
    "avif", "mp3", "wav", "flac", "ogg", "m4a", "mp4", "mkv", "webm", "mov", "avi", "zip", "gz",
    "tgz", "tar", "bz2", "xz", "zst", "7z", "rar", "docx", "xlsx", "pptx", "odt", "ods", "odp",
    "ttf", "otf", "woff", "woff2", "bin", "dat", "parquet", "patch", "diff", "download",
];

/// Names Windows reserves, whatever the extension (and trailing spaces or dots), superscript
/// digits and the console devices included.
const RESERVED: &[&str] = &[
    "con",
    "prn",
    "aux",
    "nul",
    "conin$",
    "conout$",
    "com0",
    "com1",
    "com2",
    "com3",
    "com4",
    "com5",
    "com6",
    "com7",
    "com8",
    "com9",
    "com\u{b9}",
    "com\u{b2}",
    "com\u{b3}",
    "lpt0",
    "lpt1",
    "lpt2",
    "lpt3",
    "lpt4",
    "lpt5",
    "lpt6",
    "lpt7",
    "lpt8",
    "lpt9",
    "lpt\u{b9}",
    "lpt\u{b2}",
    "lpt\u{b3}",
];

/// A file saved by [`Web::download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub path: PathBuf,
    pub bytes: u64,
    /// The server's `content-type` (lower case), or empty.
    pub content_type: String,
    /// Where the file came from after redirects.
    pub final_url: String,
    /// What the agent should know about how the file was saved (a name changed, a type that
    /// does not match its name).
    pub notes: Vec<String>,
}

fn reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default();
    let stem = stem.trim_end_matches([' ', '.']).to_lowercase();
    RESERVED.contains(&stem.as_str())
}

/// A file name made safe from whatever a page or server suggested: no folders, no characters a
/// file system treats specially, no hidden or reserved names, a bounded length, and `.download`
/// added unless the extension is a known inert one. Returns the name and whether it was changed
/// beyond trimming.
pub fn safe_name(suggested: &str) -> (String, bool) {
    let base = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    let mut name: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    name = name
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if name.is_empty() {
        name = "download".into();
    }
    if reserved(&name) {
        name.insert(0, '_');
    }
    if name.chars().count() > MAX_NAME {
        // Keep a short extension, cut the stem.
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if ext.chars().count() <= 10 => (stem.to_string(), format!(".{ext}")),
            _ => (name.clone(), String::new()),
        };
        let keep = MAX_NAME.saturating_sub(ext.chars().count());
        name = format!("{}{ext}", stem.chars().take(keep).collect::<String>());
    }
    if let Some((_, ext)) = name.rsplit_once('.')
        && !INERT.contains(&ext.to_ascii_lowercase().as_str())
    {
        name.push_str(".download");
    }
    let changed = name != suggested.trim();
    (name, changed)
}

/// Why a file of this (already safe) name is not saved at all, if it is not: files a system reads
/// settings from when it opens the folder.
fn refused_name(name: &str) -> Option<String> {
    let bare = name.strip_suffix(".download").unwrap_or(name);
    bare.eq_ignore_ascii_case("desktop.ini").then(|| {
        "desktop.ini changes how a folder behaves when it is opened, so it is not saved".to_string()
    })
}

/// The file name in a `Content-Disposition` header, if it names one (`filename*` first).
pub fn disposition_name(header: &str) -> Option<String> {
    let mut plain = None;
    for part in header.split(';').skip(1) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                // RFC 5987: charset'language'percent-encoded.
                let encoded = value.splitn(3, '\'').nth(2).unwrap_or(value);
                let decoded = percent_decode(encoded);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
            "filename" => {
                let unquoted = value.trim_matches('"');
                if !unquoted.is_empty() {
                    plain = Some(unquoted.to_string());
                }
            }
            _ => {}
        }
    }
    plain
}

/// The last path segment of `url`, percent-decoded, if there is one.
pub fn url_name(url: &Url) -> Option<String> {
    let segment = url.path_segments()?.next_back()?;
    let decoded = percent_decode(segment);
    (!decoded.is_empty()).then_some(decoded)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The extension of `name` in lower case, if it has one.
fn extension(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

/// Whether `meta` describes a link: a symbolic link, or on Windows any reparse point (a junction
/// included).
fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// A downloads folder that passed [`secure_dir`]: its canonical path and its identity on disk
/// (device and inode, or volume and file index), which a swapped folder does not share.
struct Secured {
    path: PathBuf,
    id: same_file::Handle,
}

/// The identity of the file or folder at `path`, read off the async threads.
async fn identity(path: &Path) -> std::io::Result<same_file::Handle> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || same_file::Handle::from_path(path))
        .await
        .map_err(std::io::Error::other)?
}

/// Makes `dir` if needed and returns it canonical with its identity, refusing a folder that is
/// itself a link, that resolves anywhere but into its own parent, or that is not the folder its
/// canonical path names: a downloads folder swapped for a link to somewhere else (the home
/// folder, a startup folder) must not receive files. Names are never compared as text, so case
/// and 8.3 short names need no special handling: both sides are resolved by the file system.
async fn secure_dir(dir: &Path) -> Result<Secured, WebError> {
    let fail = |message: String| WebError::Http(message);
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| fail(format!("could not make the downloads folder: {error}")))?;
    let meta = tokio::fs::symlink_metadata(dir)
        .await
        .map_err(|error| fail(format!("could not read the downloads folder: {error}")))?;
    if is_link(&meta) {
        return Err(fail(
            "the downloads folder is a link (a symbolic link or junction), so nothing is saved \
             in it"
                .into(),
        ));
    }
    let resolve =
        |error: std::io::Error| fail(format!("could not resolve the downloads folder: {error}"));
    let canonical = tokio::fs::canonicalize(dir).await.map_err(resolve)?;
    let parent = match dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let expected_parent = tokio::fs::canonicalize(&parent).await.map_err(resolve)?;
    let id = identity(&canonical).await.map_err(resolve)?;
    let given = identity(dir).await.map_err(resolve)?;
    if canonical.parent() != Some(expected_parent.as_path()) || id != given {
        return Err(fail(
            "the downloads folder resolves to another place than where it is, so nothing is \
             saved in it"
                .into(),
        ));
    }
    Ok(Secured {
        path: canonical,
        id,
    })
}

/// Refuses to go on when the folder `written` is in no longer is `secured` (its canonical path
/// and its identity): the folder was swapped after the check. This narrows the window in which a
/// swap can redirect a file; it does not close it, since the folder can still change between
/// this check and the next operation.
async fn still_secured(secured: &Secured, written: &Path) -> Result<(), WebError> {
    let moved = || {
        WebError::Http(
            "the downloads folder changed while the file was being saved, so it was not kept"
                .into(),
        )
    };
    let parent = written.parent().ok_or_else(moved)?;
    let now = tokio::fs::canonicalize(parent).await.map_err(|_| moved())?;
    let id = identity(&now).await.map_err(|_| moved())?;
    if now == secured.path && id == secured.id {
        Ok(())
    } else {
        Err(moved())
    }
}

/// Removes the files of an unfinished download when dropped, which includes the download being
/// cancelled.
struct Partial {
    paths: Vec<PathBuf>,
}

impl Drop for Partial {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Marks a file as coming from the web where the platform has a way to: the `Zone.Identifier`
/// stream on Windows (zone 3, the internet), the quarantine attribute on macOS. The mark is made
/// on the `.part` file, so it travels with the rename and the finished file never exists
/// unmarked. Returns why it could not.
async fn mark_from_web(path: &Path, source: &str) -> Result<(), String> {
    let (path, source) = (path.to_path_buf(), source.to_string());
    tokio::task::spawn_blocking(move || mark_platform(&path, &source))
        .await
        .map_err(|error| {
            format!("it could not be marked as downloaded from the internet ({error})")
        })?
}

#[cfg(windows)]
fn mark_platform(path: &Path, source: &str) -> Result<(), String> {
    let stream = format!("{}:Zone.Identifier", path.display());
    let text = format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={source}\r\n");
    std::fs::write(stream, text).map_err(|error| {
        format!("it could not be marked as downloaded from the internet ({error})")
    })
}

#[cfg(target_os = "macos")]
fn mark_platform(path: &Path, _source: &str) -> Result<(), String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let value = format!("0083;{seconds:x};Imperium;");
    let status = std::process::Command::new("xattr")
        .args(["-w", "com.apple.quarantine", value.as_str()])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err("it could not be marked as downloaded from the internet".into()),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn mark_platform(_path: &Path, _source: &str) -> Result<(), String> {
    Ok(())
}

impl Web {
    /// Downloads `url` into `dir` (made if needed) as `name_hint` (or the server's or the URL's
    /// name) made safe, never replacing a file that is there: a taken name gets ` (2)`,
    /// ` (3)` and so on. Refuses a file larger than `max_bytes` (by its announced length, and
    /// again while receiving, so a server that lies is cut off). The file is written as
    /// `<name>.part` and renamed when complete; a failed, timed-out or cancelled download leaves
    /// nothing behind. `follow` judges each redirect as for `web_fetch`; `robots.txt` is checked
    /// as for it. `refuse` is shown the resolved path the file would be saved at, before
    /// anything is written, and returns why that must not happen (the safety floor's protected
    /// files, a path the session may not write).
    pub async fn download(
        &self,
        url: &str,
        dir: &Path,
        name_hint: Option<&str>,
        max_bytes: u64,
        follow: &(dyn Fn(&Url) -> bool + Sync),
        refuse: &(dyn Fn(&Path) -> Option<String> + Sync),
    ) -> Result<Downloaded, WebError> {
        self.download_within(
            url,
            dir,
            name_hint,
            max_bytes,
            follow,
            refuse,
            DOWNLOAD_DEADLINE,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_within(
        &self,
        url: &str,
        dir: &Path,
        name_hint: Option<&str>,
        max_bytes: u64,
        follow: &(dyn Fn(&Url) -> bool + Sync),
        refuse: &(dyn Fn(&Path) -> Option<String> + Sync),
        deadline: Duration,
    ) -> Result<Downloaded, WebError> {
        let all = self.download_unbounded(url, dir, name_hint, max_bytes, follow, refuse);
        match tokio::time::timeout(deadline, all).await {
            Ok(outcome) => outcome,
            Err(_) => Err(WebError::Http(format!(
                "the download took longer than {} seconds and was stopped",
                deadline.as_secs()
            ))),
        }
    }

    /// The whole download, from the request through the redirects and headers to the rename;
    /// [`Self::download_within`] bounds all of it.
    async fn download_unbounded(
        &self,
        url: &str,
        dir: &Path,
        name_hint: Option<&str>,
        max_bytes: u64,
        follow: &(dyn Fn(&Url) -> bool + Sync),
        refuse: &(dyn Fn(&Path) -> Option<String> + Sync),
    ) -> Result<Downloaded, WebError> {
        let parsed = Url::parse(url)
            .ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https") && parsed.has_host())
            .ok_or_else(|| WebError::BadUrl(url.into()))?;
        let (mut response, _) = self
            .get_following_with(&self.download_http, &parsed, follow)
            .await?;
        let final_url = response.url().clone();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        let content_type = header("content-type")
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let suggested = name_hint
            .map(str::to_string)
            .filter(|hint| !hint.trim().is_empty())
            .or_else(|| disposition_name(&header("content-disposition")))
            .or_else(|| url_name(&final_url))
            .unwrap_or_default();
        if let Some(length) = response.content_length()
            && length > max_bytes
        {
            return Err(WebError::Http(format!(
                "the file is {length} bytes, more than the {max_bytes} allowed (web.download_max_mb)"
            )));
        }
        let (name, changed) = safe_name(&suggested);
        if let Some(why) = refused_name(&name) {
            return Err(WebError::Http(why));
        }
        let mut notes = Vec::new();
        if changed && !suggested.trim().is_empty() {
            notes.push(format!(
                "the suggested name {:?} was changed to {name:?} so it is a safe file name",
                crate::tabs::one_line(&suggested, 80)
            ));
        }
        if content_type == "text/html"
            && !matches!(
                extension(&name).as_deref(),
                Some("html" | "htm" | "download")
            )
        {
            notes.push(
                "the server sent a web page, not the file the name suggests (an error or login \
                 page, perhaps); read it before relying on it"
                    .to_string(),
            );
        }
        let secured = secure_dir(dir).await?;
        let dir = secured.path.clone();
        if let Some(why) = refuse(&dir.join(&name)) {
            return Err(WebError::Http(why));
        }
        let (path, reserved_file) = create_unique(&dir, &name).await?;
        drop(reserved_file);
        let mut part_name = path.file_name().unwrap_or_default().to_os_string();
        part_name.push(".part");
        let part = path.with_file_name(part_name);
        let mut guard = Partial {
            paths: vec![path.clone(), part.clone()],
        };
        let mut file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
        {
            Ok(file) => file,
            Err(error) => {
                return Err(WebError::Http(format!(
                    "could not create the file: {error}"
                )));
            }
        };
        let mut written: u64 = 0;
        let transfer = async {
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| WebError::Http(error.to_string()))?
            {
                written += chunk.len() as u64;
                if written > max_bytes {
                    return Err(WebError::Http(format!(
                        "the file is larger than the {max_bytes} bytes allowed (web.download_max_mb); \
                         the download was stopped"
                    )));
                }
                file.write_all(&chunk).await.map_err(|error| {
                    WebError::Http(format!("could not write the file: {error}"))
                })?;
            }
            file.flush()
                .await
                .map_err(|error| WebError::Http(format!("could not write the file: {error}")))
        };
        transfer.await?;
        drop(file);
        // The folder was checked before the file was made; check again that it still is that
        // folder before anything more is done to the part file, and again once it is in place.
        still_secured(&secured, &part).await?;
        // The mark goes on the part file so the finished file is never there without it. On
        // Windows a file that cannot be marked (a folder on FAT, exFAT or some network shares
        // cannot hold the stream) is not kept; elsewhere it is saved with a note.
        if let Err(why) = mark_from_web(&part, final_url.as_str()).await {
            if cfg!(windows) {
                return Err(WebError::Http(format!("the file was not kept: {why}")));
            }
            notes.push(why);
        }
        still_secured(&secured, &part).await?;
        // The part file's identity, taken just before it is moved, so that what is found at the
        // destination afterwards can be told from something else.
        let part_id = identity(&part).await.ok();
        tokio::fs::rename(&part, &path)
            .await
            .map_err(|error| WebError::Http(format!("could not finish the file: {error}")))?;
        if let Err(error) = still_secured(&secured, &path).await {
            // It landed where it should not have: take it back, but only if what is there is
            // the file just moved; anything else is not ours to delete.
            let ours = match (&part_id, identity(&path).await) {
                (Some(before), Ok(now)) => *before == now,
                _ => false,
            };
            // The guard must not remove it either: that is decided here.
            guard.paths.clear();
            return Err(if ours {
                let _ = tokio::fs::remove_file(&path).await;
                error
            } else {
                WebError::Http(format!(
                    "{error}; the file at {} was left in place because it could not be \
                     confirmed to be the one just saved",
                    path.display()
                ))
            });
        }
        guard.paths.clear();
        if path.file_name().and_then(|file| file.to_str()) != Some(name.as_str()) {
            notes.push("a file with that name already existed, so this one has another".into());
        }
        Ok(Downloaded {
            path,
            bytes: written,
            content_type,
            final_url: final_url.to_string(),
            notes,
        })
    }
}

/// Creates `name` in `dir`, or `name (2)`, `name (3)` and so on if it is taken, without ever
/// opening an existing file.
async fn create_unique(dir: &Path, name: &str) -> Result<(PathBuf, tokio::fs::File), WebError> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    };
    for number in 1..=200u32 {
        let candidate = if number == 1 {
            name.to_string()
        } else {
            format!("{stem} ({number}){ext}")
        };
        let path = dir.join(&candidate);
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(WebError::Http(format!(
                    "could not create the file: {error}"
                )));
            }
        }
    }
    Err(WebError::Http(
        "too many files with that name in the downloads folder".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        url
    }

    fn web() -> Web {
        Web::with_options(
            None,
            crate::FetchOptions {
                respect_robots: false,
                ..Default::default()
            },
        )
    }

    #[test]
    fn names_lose_folders_and_special_characters() {
        let name = |text: &str| safe_name(text).0;
        assert_eq!(name("report.pdf"), "report.pdf");
        assert_eq!(name("../../etc/passwd"), "passwd");
        assert_eq!(name("C:\\Users\\me\\x.txt"), "x.txt");
        assert_eq!(name("a<b>:c|d?e*.txt"), "a_b__c_d_e_.txt");
        assert_eq!(name("  spaced name .txt  "), "spaced name .txt");
        assert_eq!(name(".bashrc"), "bashrc");
        assert_eq!(name("..."), "download");
        assert_eq!(name(""), "download");
        assert_eq!(name("line\nbreak\u{7}.txt"), "line_break_.txt");
        assert_eq!(name("CON.txt"), "_CON.txt");
        assert_eq!(name("nul"), "_nul");
        assert_eq!(name("com1.log"), "_com1.log");
        assert_eq!(name("console.log"), "console.log");
        // Reserved names the simple check missed: more devices, superscript digits, trailing
        // spaces before the extension.
        assert_eq!(name("COM0.txt"), "_COM0.txt");
        assert_eq!(name("lpt\u{b9}.txt"), "_lpt\u{b9}.txt");
        assert_eq!(name("CONIN$"), "_CONIN$");
        assert_eq!(name("CONOUT$.txt"), "_CONOUT$.txt");
        assert_eq!(name("con .txt"), "_con .txt");
        assert_eq!(name("aux.tar.gz"), "_aux.tar.gz");
    }

    #[test]
    fn only_inert_extensions_keep_their_names() {
        for inert in [
            "a.txt",
            "a.PDF",
            "a.zip",
            "a.png",
            "a.csv",
            "a.docx",
            "a.json",
            "noextension",
        ] {
            assert_eq!(safe_name(inert).0, inert, "{inert}");
        }
        // Everything that opens, mounts, loads or launches something, and anything unknown.
        for risky in [
            "a.scf",
            "a.library-ms",
            "a.searchConnector-ms",
            "a.url",
            "a.appref-ms",
            "a.application",
            "a.settingcontent-ms",
            "a.msc",
            "a.chm",
            "a.inf",
            "a.xll",
            "a.msix",
            "a.appx",
            "a.msp",
            "a.wsc",
            "a.sct",
            "a.jnlp",
            "a.iso",
            "a.img",
            "a.vhd",
            "a.vhdx",
            "a.docm",
            "a.xlsm",
            "a.exe",
            "a.lnk",
            "a.sh",
            "a.something-new",
        ] {
            assert_eq!(safe_name(risky).0, format!("{risky}.download"), "{risky}");
        }
        // Already marked: not marked twice.
        assert_eq!(safe_name("a.exe.download").0, "a.exe.download");
    }

    #[test]
    fn files_a_person_could_run_get_a_suffix_so_nothing_opens_them_by_accident() {
        for runnable in [
            "setup.exe",
            "run.BAT",
            "x.sh",
            "evil.ps1",
            "app.AppImage",
            "t.py",
        ] {
            let (name, changed) = safe_name(runnable);
            assert_eq!(name, format!("{runnable}.download"));
            assert!(changed);
        }
        assert_eq!(safe_name("notes.txt").0, "notes.txt");
        assert_eq!(safe_name("data.json").0, "data.json");
        // The extension is the last one: a runnable name inside a longer one is still caught.
        assert_eq!(safe_name("a.exe.txt").0, "a.exe.txt");
        assert_eq!(safe_name("a.txt.exe").0, "a.txt.exe.download");
    }

    #[test]
    fn long_names_keep_their_extension() {
        let (name, changed) = safe_name(&format!("{}.pdf", "a".repeat(300)));
        assert_eq!(name.chars().count(), MAX_NAME);
        assert!(name.ends_with(".pdf"));
        assert!(changed);
        let (name, _) = safe_name(&"b".repeat(300));
        assert_eq!(name.chars().count(), MAX_NAME);
    }

    #[test]
    fn disposition_and_url_names_are_read() {
        assert_eq!(
            disposition_name("attachment; filename=\"a b.pdf\"").as_deref(),
            Some("a b.pdf")
        );
        assert_eq!(
            disposition_name("attachment; filename=plain.txt").as_deref(),
            Some("plain.txt")
        );
        assert_eq!(
            disposition_name(
                "attachment; filename=\"fallback.txt\"; filename*=UTF-8''na%C3%AFve%20file.txt"
            )
            .as_deref(),
            Some("naïve file.txt")
        );
        assert_eq!(disposition_name("inline"), None);
        assert_eq!(disposition_name(""), None);
        let url = Url::parse("https://x.example/dir/some%20file.zip?token=1").unwrap();
        assert_eq!(url_name(&url).as_deref(), Some("some file.zip"));
        assert_eq!(url_name(&Url::parse("https://x.example/").unwrap()), None);
    }

    #[tokio::test]
    async fn a_file_is_saved_under_a_safe_name_and_never_replaces_another() {
        let base = serve(Router::new().route(
            "/get",
            get(|| async {
                (
                    [
                        ("content-type", "application/pdf"),
                        (
                            "content-disposition",
                            "attachment; filename=\"../../evil.pdf\"",
                        ),
                    ],
                    vec![7u8; 1000],
                )
            }),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("downloads");
        let web = web();
        let first = web
            .download(
                &format!("{base}/get"),
                &downloads,
                None,
                10_000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        assert_eq!(
            first.path,
            downloads.canonicalize().unwrap().join("evil.pdf")
        );
        assert_eq!(first.bytes, 1000);
        assert_eq!(first.content_type, "application/pdf");
        assert_eq!(std::fs::read(&first.path).unwrap(), vec![7u8; 1000]);
        assert!(
            first
                .notes
                .iter()
                .any(|note| note.contains("safe file name"))
        );
        let second = web
            .download(
                &format!("{base}/get"),
                &downloads,
                None,
                10_000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        assert_eq!(
            second.path,
            downloads.canonicalize().unwrap().join("evil (2).pdf")
        );
        assert!(
            second
                .notes
                .iter()
                .any(|note| note.contains("already existed"))
        );
        assert_eq!(std::fs::read_dir(&downloads).unwrap().count(), 2);
        // A name the agent chose is made safe too.
        let named = web
            .download(
                &format!("{base}/get"),
                &downloads,
                Some("../x/run.sh"),
                10_000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        assert_eq!(
            named.path,
            downloads.canonicalize().unwrap().join("run.sh.download")
        );
    }

    #[tokio::test]
    async fn a_file_over_the_cap_is_refused_announced_or_not_and_leaves_nothing_behind() {
        let base = serve(
            Router::new()
                .route(
                    "/big",
                    get(|| async { ([("content-type", "application/zip")], vec![1u8; 5000]) }),
                )
                .route(
                    "/stream",
                    get(|| async {
                        let chunks = (0..50).map(|_| Ok::<_, std::io::Error>(vec![2u8; 100]));
                        axum::body::Body::from_stream(futures_util::stream::iter(chunks))
                    }),
                ),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let web = web();
        let error = web
            .download(
                &format!("{base}/big"),
                dir.path(),
                None,
                1000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than the 1000 allowed"), "{error}");
        // No announced length: cut off while receiving.
        let error = web
            .download(
                &format!("{base}/stream"),
                dir.path(),
                None,
                1000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("larger than the 1000 bytes"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_web_page_served_as_a_file_is_flagged_and_other_schemes_are_refused() {
        let base = serve(Router::new().route(
            "/report.pdf",
            get(|| async {
                (
                    [("content-type", "text/html; charset=utf-8")],
                    "<html>login</html>",
                )
            }),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let web = web();
        let saved = web
            .download(
                &format!("{base}/report.pdf"),
                dir.path(),
                None,
                10_000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        assert_eq!(saved.content_type, "text/html");
        assert!(
            saved.notes.iter().any(|note| note.contains("a web page")),
            "{:?}",
            saved.notes
        );
        for bad in [
            "file:///etc/passwd",
            "ftp://x/y",
            "blob:http://x/1",
            "data:text/plain,hi",
            "nonsense",
        ] {
            assert!(
                matches!(
                    web.download(bad, dir.path(), None, 10, &|_| true, &|_| None)
                        .await,
                    Err(WebError::BadUrl(_))
                ),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn redirects_and_robots_follow_the_fetch_rules() {
        let target = serve(Router::new().route("/f.txt", get(|| async { "secret" }))).await;
        let to = format!("{target}/f.txt");
        let base = serve(
            Router::new()
                .route(
                    "/go",
                    get(move || {
                        let to = to.clone();
                        async move { axum::response::Redirect::temporary(&to) }
                    }),
                )
                .route(
                    "/robots.txt",
                    get(|| async { "User-agent: *\nDisallow: /private/\n" }),
                )
                .route("/private/a.bin", get(|| async { "x" })),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        // A redirect the caller does not follow is not requested.
        let error = web()
            .download(
                &format!("{base}/go"),
                dir.path(),
                None,
                100,
                &|_| false,
                &|_| None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("not followed"), "{error}");
        let saved = web()
            .download(
                &format!("{base}/go"),
                dir.path(),
                None,
                100,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        assert_eq!(saved.path, dir.path().canonicalize().unwrap().join("f.txt"));
        // robots.txt applies as for web_fetch (the default), and `off` skips it.
        let respecting = Web::new(None);
        let error = respecting
            .download(
                &format!("{base}/private/a.bin"),
                dir.path(),
                None,
                100,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, WebError::Robots(_)), "{error}");
        assert!(
            web()
                .download(
                    &format!("{base}/private/a.bin"),
                    dir.path(),
                    None,
                    100,
                    &|_| true,
                    &|_| None
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn desktop_ini_is_not_saved() {
        let base =
            serve(Router::new().route("/desktop.ini", get(|| async { "[.ShellClassInfo]" }))).await;
        let dir = tempfile::tempdir().unwrap();
        let error = web()
            .download(
                &format!("{base}/desktop.ini"),
                dir.path(),
                None,
                1000,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("desktop.ini"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_download_reports_where_redirects_ended_and_leaves_no_partial_file() {
        let target = serve(Router::new().route("/final.txt", get(|| async { "done" }))).await;
        let to = format!("{target}/final.txt");
        let base = serve(Router::new().route(
            "/go",
            get(move || {
                let to = to.clone();
                async move { axum::response::Redirect::temporary(&to) }
            }),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let saved = web()
            .download(
                &format!("{base}/go"),
                dir.path(),
                None,
                100,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        assert_eq!(saved.final_url, format!("{target}/final.txt"));
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, ["final.txt"], "no .part file is left");
    }

    /// A body that sends a little and then waits.
    fn stalling() -> axum::body::Body {
        let stream = futures_util::stream::unfold(0u32, |step| async move {
            if step > 0 {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Some((Ok::<_, std::io::Error>(vec![3u8; 100]), step + 1))
        });
        axum::body::Body::from_stream(stream)
    }

    #[tokio::test]
    async fn a_download_that_takes_too_long_is_stopped_and_leaves_nothing() {
        let base = serve(Router::new().route("/slow.bin", get(|| async { stalling() }))).await;
        let dir = tempfile::tempdir().unwrap();
        let error = web()
            .download_within(
                &format!("{base}/slow.bin"),
                dir.path(),
                None,
                10_000,
                &|_| true,
                &|_| None,
                Duration::from_millis(500),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("took longer"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn the_deadline_covers_a_server_slow_to_answer() {
        let base = serve(Router::new().route(
            "/late.bin",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "x"
            }),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let error = web()
            .download_within(
                &format!("{base}/late.bin"),
                dir.path(),
                None,
                10_000,
                &|_| true,
                &|_| None,
                Duration::from_millis(500),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("took longer"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_folder_named_in_another_case_is_accepted_on_windows() {
        let root = tempfile::tempdir().unwrap();
        let long = root.path().join("a long downloads folder");
        std::fs::create_dir(&long).unwrap();
        let shout = root.path().join("A LONG DOWNLOADS FOLDER");
        let secured = secure_dir(&shout).await.unwrap();
        assert_eq!(secured.path, long.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn the_folder_is_checked_again_before_the_file_is_put_in_place() {
        let root = tempfile::tempdir().unwrap();
        let secured = secure_dir(&root.path().join("downloads")).await.unwrap();
        let part = secured.path.join("f.txt.part");
        assert!(still_secured(&secured, &part).await.is_ok());
        // Another folder than the secured one is refused, as is one that has gone.
        let other = secure_dir(&root.path().join("other")).await.unwrap();
        let error = still_secured(&secured, &other.path.join("f.txt.part"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("changed while"), "{error}");
        std::fs::remove_dir(&secured.path).unwrap();
        assert!(still_secured(&secured, &part).await.is_err());
    }

    #[tokio::test]
    async fn a_folder_has_the_identity_of_the_place_it_names() {
        let root = tempfile::tempdir().unwrap();
        let first = secure_dir(&root.path().join("a")).await.unwrap();
        let second = secure_dir(&root.path().join("b")).await.unwrap();
        assert!(first.id != second.id);
        let again = secure_dir(&root.path().join("a")).await.unwrap();
        assert!(first.id == again.id);
        assert_eq!(first.path, again.path);
    }

    #[tokio::test]
    async fn a_cancelled_download_removes_its_partial_file() {
        let base = serve(Router::new().route("/slow.bin", get(|| async { stalling() }))).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let url = format!("{base}/slow.bin");
        let task = tokio::spawn(async move {
            web()
                .download(&url, &path, None, 10_000, &|_| true, &|_| None)
                .await
        });
        // Wait until the partial file exists, then cancel.
        for _ in 0..100 {
            if std::fs::read_dir(dir.path()).unwrap().count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(std::fs::read_dir(dir.path()).unwrap().count() > 0);
        task.abort();
        let _ = task.await;
        for _ in 0..100 {
            if std::fs::read_dir(dir.path()).unwrap().count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_downloads_folder_that_is_a_link_receives_nothing() {
        let base = serve(Router::new().route("/f.txt", get(|| async { "x" }))).await;
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let link = root.path().join("downloads");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&elsewhere)
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "could not make a junction");
        }
        let error = web()
            .download(
                &format!("{base}/f.txt"),
                &link,
                None,
                100,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("is a link"), "{error}");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        // The refusal sees the resolved path: a real folder is passed to it in canonical form.
        let real = root.path().join("real");
        let seen = std::sync::Mutex::new(None);
        web()
            .download(
                &format!("{base}/f.txt"),
                &real,
                None,
                100,
                &|_| true,
                &|path| {
                    *seen.lock().unwrap() = Some(path.to_path_buf());
                    None
                },
            )
            .await
            .unwrap();
        let seen = seen.lock().unwrap().clone().unwrap();
        assert_eq!(seen.parent().unwrap(), real.canonicalize().unwrap());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_download_on_windows_is_marked_as_from_the_internet() {
        let base = serve(Router::new().route("/f.txt", get(|| async { "x" }))).await;
        let dir = tempfile::tempdir().unwrap();
        let saved = web()
            .download(
                &format!("{base}/f.txt"),
                dir.path(),
                None,
                100,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        let zone =
            std::fs::read_to_string(format!("{}:Zone.Identifier", saved.path.display())).unwrap();
        assert!(zone.contains("ZoneId=3"), "{zone}");
        assert!(zone.contains(&format!("HostUrl={base}/f.txt")), "{zone}");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_download_on_macos_is_quarantined() {
        let base = serve(Router::new().route("/f.txt", get(|| async { "x" }))).await;
        let dir = tempfile::tempdir().unwrap();
        let saved = web()
            .download(
                &format!("{base}/f.txt"),
                dir.path(),
                None,
                100,
                &|_| true,
                &|_| None,
            )
            .await
            .unwrap();
        let output = std::process::Command::new("xattr")
            .args(["-p", "com.apple.quarantine"])
            .arg(&saved.path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?} {:?}", saved.notes, output);
    }
}
