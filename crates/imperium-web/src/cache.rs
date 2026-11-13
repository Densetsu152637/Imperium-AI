//! A bounded on-disk cache of pages `web_fetch` has read, keyed by URL.
//!
//! One JSON file per page in one folder. The cache never exceeds its size cap (the oldest
//! files go first) and entries expire after their lifetime. `web_fetch` sends no cookies and no
//! credentials, so a cached page is what any visitor sees; even so, a response is stored only
//! when [`cacheable_lifetime`] says it may be shared: no `Set-Cookie`, no `Cache-Control:
//! no-store`, `private` or `no-cache`, no `WWW-Authenticate`, no `Vary` on a cookie or an
//! authorization header, and no credentials in the URL.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{CACHE_CONTROL, HeaderMap, SET_COOKIE, VARY, WWW_AUTHENTICATE};
use serde::{Deserialize, Serialize};
use url::Url;

/// A fetched page as `web_fetch` keeps it: the text is whole (not one chunk) so later
/// `offset` calls are answered from the cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub title: Option<String>,
    pub final_url: String,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    url: String,
    expires_at: u64,
    #[serde(flatten)]
    page: Page,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The cache folder with its lifetime and size cap.
#[derive(Debug, Clone)]
pub struct FetchCache {
    dir: PathBuf,
    ttl: Duration,
    max_bytes: u64,
}

impl FetchCache {
    /// A cache in `dir`, or `None` when `ttl` or `max_bytes` is zero (caching off).
    pub fn new(dir: PathBuf, ttl: Duration, max_bytes: u64) -> Option<Self> {
        (!ttl.is_zero() && max_bytes > 0).then_some(FetchCache {
            dir,
            ttl,
            max_bytes,
        })
    }

    /// The configured lifetime, the longest any entry lives.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The cache key for `url`: the URL without its fragment. Credentials in the URL, or a
    /// URL that does not parse, are never cached (`None`).
    pub fn key(url: &Url) -> Option<String> {
        if !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let mut url = url.clone();
        url.set_fragment(None);
        Some(url.into())
    }

    fn file(&self, key: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        self.dir.join(format!("{:016x}.json", hasher.finish()))
    }

    /// The page stored for `key` if it is still fresh. Expired or unreadable files are deleted.
    pub fn get(&self, key: &str) -> Option<Page> {
        let file = self.file(key);
        let bytes = fs::read(&file).ok()?;
        match serde_json::from_slice::<Entry>(&bytes) {
            Ok(entry) if entry.url == key && entry.expires_at > now() => Some(entry.page),
            // A hash collision keeps the other URL's entry.
            Ok(entry) if entry.url != key => None,
            _ => {
                let _ = fs::remove_file(&file);
                None
            }
        }
    }

    /// Stores `page` for `key` for `lifetime` (at most the configured one), then trims the
    /// folder to its cap. Failures are ignored: the cache only ever saves a request.
    pub fn put(&self, key: &str, page: &Page, lifetime: Duration) {
        let lifetime = lifetime.min(self.ttl);
        if lifetime.is_zero() {
            return;
        }
        let entry = Entry {
            url: key.to_string(),
            expires_at: now() + (lifetime.as_millis() as u64).max(1),
            page: page.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&entry) else {
            return;
        };
        if bytes.len() as u64 > self.max_bytes || fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let file = self.file(key);
        let temporary = file.with_extension("tmp");
        if fs::write(&temporary, &bytes).is_ok() && fs::rename(&temporary, &file).is_err() {
            let _ = fs::remove_file(&temporary);
        }
        self.trim();
    }

    /// Deletes files older than the lifetime, then the oldest until the folder fits its cap.
    fn trim(&self) {
        let Ok(listing) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files = Vec::new();
        let mut total = 0u64;
        for item in listing.flatten() {
            let path = item.path();
            let Ok(meta) = item.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "json") {
                // A leftover temporary file from an interrupted write.
                let _ = fs::remove_file(&path);
                continue;
            }
            // No entry lives longer than the configured lifetime from when it was written.
            let modified = meta.modified().unwrap_or(UNIX_EPOCH);
            if modified + self.ttl <= SystemTime::now() {
                let _ = fs::remove_file(&path);
                continue;
            }
            total += meta.len();
            files.push((modified, meta.len(), path));
        }
        files.sort();
        for (_, length, path) in files {
            if total <= self.max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= length;
            }
        }
    }
}

/// How long a response may be reused, or `None` when it must not be stored at all. The
/// shared-cache rules: only a 200 answer; nothing that sets a cookie, asks for authentication,
/// varies on who is asking, or says `no-store`, `private` or `no-cache`; `max-age` (or
/// `s-maxage`) of zero or less than the configured lifetime shortens it.
pub fn cacheable_lifetime(
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    ttl: Duration,
) -> Option<Duration> {
    if status != reqwest::StatusCode::OK
        || headers.contains_key(SET_COOKIE)
        || headers.contains_key(WWW_AUTHENTICATE)
    {
        return None;
    }
    let joined = |name: reqwest::header::HeaderName| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>()
            .join(",")
            .to_ascii_lowercase()
    };
    let vary = joined(VARY);
    if vary
        .split(',')
        .map(str::trim)
        .any(|field| matches!(field, "*" | "cookie" | "authorization"))
    {
        return None;
    }
    let mut lifetime = ttl;
    for directive in joined(CACHE_CONTROL).split(',').map(str::trim) {
        let (name, value) = directive.split_once('=').unwrap_or((directive, ""));
        match name.trim() {
            "no-store" | "private" | "no-cache" => return None,
            "max-age" | "s-maxage" => {
                let seconds = value.trim().trim_matches('"').parse::<u64>().ok()?;
                lifetime = lifetime.min(Duration::from_secs(seconds));
            }
            _ => {}
        }
    }
    (!lifetime.is_zero()).then_some(lifetime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use reqwest::header::HeaderValue;

    fn page(text: &str) -> Page {
        Page {
            title: Some("T".into()),
            final_url: "https://example.com/final".into(),
            text: text.into(),
        }
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn pages_round_trip_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let cache =
            FetchCache::new(dir.path().join("web"), Duration::from_secs(60), 1 << 20).expect("on");
        assert_eq!(cache.get("https://a/"), None);
        cache.put("https://a/", &page("hello"), Duration::from_secs(60));
        assert_eq!(cache.get("https://a/"), Some(page("hello")));
        assert_eq!(cache.get("https://b/"), None, "keyed by URL");
        // An entry written already expired is dropped when read.
        let file = cache.file("https://a/");
        let stale = Entry {
            url: "https://a/".into(),
            expires_at: 1,
            page: page("old"),
        };
        fs::write(&file, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert_eq!(cache.get("https://a/"), None);
        assert!(!file.exists());
        // Corrupt files are dropped too.
        fs::write(&file, b"not json").unwrap();
        assert_eq!(cache.get("https://a/"), None);
        assert!(!file.exists());
    }

    #[test]
    fn the_folder_stays_under_its_cap_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache =
            FetchCache::new(dir.path().to_path_buf(), Duration::from_secs(600), 3000).expect("on");
        let text = "x".repeat(900);
        for index in 0..8 {
            cache.put(
                &format!("https://a/{index}"),
                &page(&text),
                Duration::from_secs(600),
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let size: u64 = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|item| item.metadata().unwrap().len())
            .sum();
        assert!(size <= 3000, "{size} bytes");
        assert!(cache.get("https://a/7").is_some(), "the newest is kept");
        assert!(cache.get("https://a/0").is_none(), "the oldest went first");
        // A page larger than the whole cap is not stored.
        cache.put(
            "https://a/big",
            &page(&"y".repeat(5000)),
            Duration::from_secs(60),
        );
        assert!(cache.get("https://a/big").is_none());
    }

    #[test]
    fn zero_settings_turn_the_cache_off() {
        let dir = PathBuf::from("unused");
        assert!(FetchCache::new(dir.clone(), Duration::ZERO, 10).is_none());
        assert!(FetchCache::new(dir, Duration::from_secs(1), 0).is_none());
    }

    #[test]
    fn keys_drop_fragments_and_refuse_credentials() {
        let key = |url: &str| FetchCache::key(&Url::parse(url).unwrap());
        assert_eq!(
            key("https://example.com/a?b=1#frag").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(key("https://user:pw@example.com/a"), None);
        assert_eq!(key("https://user@example.com/a"), None);
    }

    #[test]
    fn only_shareable_responses_are_cacheable() {
        let ttl = Duration::from_secs(900);
        let ok = StatusCode::OK;
        assert_eq!(cacheable_lifetime(ok, &headers(&[]), ttl), Some(ttl));
        assert_eq!(
            cacheable_lifetime(
                ok,
                &headers(&[("cache-control", "public, max-age=60")]),
                ttl
            ),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            cacheable_lifetime(ok, &headers(&[("cache-control", "max-age=99999")]), ttl),
            Some(ttl),
            "the configured lifetime is the ceiling"
        );
        for refused in [
            &[("cache-control", "no-store")][..],
            &[("cache-control", "max-age=60, private")],
            &[("cache-control", "No-Cache")],
            &[("cache-control", "max-age=0")],
            &[("set-cookie", "sid=1")],
            &[("www-authenticate", "Basic")],
            &[("vary", "Accept-Encoding, Cookie")],
            &[("vary", "*")],
            &[("vary", "Authorization")],
        ] {
            assert_eq!(
                cacheable_lifetime(ok, &headers(refused), ttl),
                None,
                "{refused:?}"
            );
        }
        assert_eq!(
            cacheable_lifetime(StatusCode::PARTIAL_CONTENT, &headers(&[]), ttl),
            None
        );
        assert_eq!(
            cacheable_lifetime(ok, &headers(&[("vary", "Accept-Encoding")]), ttl),
            Some(ttl)
        );
    }
}
