//! Saving a file a page offered (`browser_download`): into one folder chosen by the session (never
//! by the page), under a name made safe, within a size cap, and never executed or marked
//! executable. The request goes through the same client, `robots.txt` check and redirect rules as
//! `web_fetch`.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use url::Url;

use crate::{Web, WebError};

/// Longest file name kept, in characters.
const MAX_NAME: usize = 100;

/// Extensions of files a person might run by opening them. A download with one is saved with
/// `.download` added, so nothing runs it by accident; its content is untouched.
const RUNNABLE: &[&str] = &[
    "exe", "msi", "bat", "cmd", "com", "scr", "pif", "ps1", "psm1", "vbs", "vbe", "js", "jse",
    "wsf", "wsh", "hta", "cpl", "lnk", "reg", "dll", "sys", "jar", "apk", "app", "dmg", "pkg",
    "deb", "rpm", "sh", "bash", "zsh", "command", "run", "appimage", "desktop", "scpt", "py", "pl",
    "rb",
];

/// Names Windows reserves, whatever the extension.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
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

/// A file name made safe from whatever a page or server suggested: no folders, no characters a
/// file system treats specially, no hidden or reserved names, a bounded length, and, for a file
/// a person could run, `.download` added. Returns the name and whether it was changed beyond
/// trimming.
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
    let stem_of = |name: &str| {
        name.split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    if RESERVED.contains(&stem_of(&name).as_str()) {
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
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    if RUNNABLE.contains(&extension.as_str()) {
        name.push_str(".download");
    }
    let changed = name != suggested.trim();
    (name, changed)
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

impl Web {
    /// Downloads `url` into `dir` (made if needed) as `name_hint` (or the server's or the URL's
    /// name) made safe, never replacing a file that is there: a taken name gets ` (2)`,
    /// ` (3)` and so on. Refuses a file larger than `max_bytes` (by its announced length, and
    /// again while receiving, so a server that lies is cut off and its partial file removed).
    /// `follow` judges each redirect as for `web_fetch`; `robots.txt` is checked as for it.
    /// `refuse` is shown the path the file would be saved at, before anything is written, and
    /// returns why that must not happen (the safety floor's protected files, for one).
    pub async fn download(
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
        let (mut response, _) = self.get_following(&parsed, follow).await?;
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
        if let Some(why) = refuse(&dir.join(&name)) {
            return Err(WebError::Http(why));
        }
        tokio::fs::create_dir_all(dir).await.map_err(|error| {
            WebError::Http(format!("could not make the downloads folder: {error}"))
        })?;
        let (path, mut file) = create_unique(dir, &name).await?;
        let mut written: u64 = 0;
        let outcome: Result<(), WebError> = async {
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
                file.write_all(&chunk)
                    .await
                    .map_err(|error| WebError::Http(format!("could not write the file: {error}")))?;
            }
            file.flush()
                .await
                .map_err(|error| WebError::Http(format!("could not write the file: {error}")))
        }
        .await;
        drop(file);
        if let Err(error) = outcome {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error);
        }
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
        assert_eq!(first.path, downloads.join("evil.pdf"));
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
        assert_eq!(second.path, downloads.join("evil (2).pdf"));
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
        assert_eq!(named.path, downloads.join("run.sh.download"));
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
        assert_eq!(saved.path, dir.path().join("f.txt"));
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
}
