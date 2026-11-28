//! Archived copies of pages that block plain HTTP, from the Wayback Machine's availability API.
//! The copy is always labelled as archived, with its date, by the caller.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use reqwest::header::LOCATION;
use serde_json::Value;
use url::Url;

use crate::cache::{FetchCache, Page};
use crate::retry::send_retrying;
use crate::{ARCHIVE_PAGE_BYTES, MAX_PAGE_CHARS, Web, block, html, read_capped};

/// Redirects followed inside the archive.
const ARCHIVE_REDIRECTS: usize = 5;

/// Name endings that mean a private network, never the public web.
const PRIVATE_SUFFIXES: [&str; 6] = [
    ".local",
    ".internal",
    ".corp",
    ".lan",
    ".home.arpa",
    ".localhost",
];

/// Why `url` must not be sent to a third-party archive, or `None` when it may be: the archive
/// is only asked about public pages, so a URL with credentials, a single-label or private-network
/// name (`.local`, `.internal`, `.corp`, `.lan`, `.home.arpa`), a private, loopback or
/// link-local address, and a name that resolves to one, are all refused.
pub(crate) async fn refusal(url: &Url, allow_private: bool) -> Option<String> {
    if FetchCache::key(url).is_none() {
        return Some("the URL carries credentials".into());
    }
    if allow_private {
        return None;
    }
    let private = "it is a private or local address, not a public page";
    match url.host()? {
        url::Host::Ipv4(ip) => non_public(IpAddr::V4(ip)).then(|| private.to_string()),
        url::Host::Ipv6(ip) => non_public(IpAddr::V6(ip)).then(|| private.to_string()),
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if !name.contains('.') || PRIVATE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
                return Some(private.into());
            }
            let port = url.port_or_known_default().unwrap_or(443);
            let lookup = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::lookup_host((name.as_str(), port)),
            )
            .await;
            match lookup {
                Ok(Ok(mut addresses)) => addresses
                    .any(|address| non_public(address.ip()))
                    .then(|| "its name resolves to a private or local address".to_string()),
                _ => Some("its name could not be resolved".into()),
            }
        }
    }
}

/// Whether `ip` is loopback, private, link-local, unspecified, shared or otherwise not on the
/// public internet.
fn non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => non_public_v4(ip),
        IpAddr::V6(ip) => non_public_v6(ip),
    }
}

fn non_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || a >= 240
}

fn non_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return non_public_v4(mapped);
    }
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}

/// An archived page.
pub(crate) struct Archived {
    pub(crate) page: Page,
    /// When the archive captured it, as `YYYY-MM-DD`.
    pub(crate) date: String,
    /// The archive's address for the capture.
    pub(crate) url: String,
}

impl Web {
    /// The archive's closest capture of `url`, looked up at `base` (`{base}/wayback/available`).
    /// The capture's address is built from the archive's date and `url`, not taken from its
    /// answer, and redirects never leave the archive's own origin. A capture that is itself a
    /// block page is refused.
    pub(crate) async fn archived_copy(&self, base: &str, url: &Url) -> Result<Archived, String> {
        let base = base.trim_end_matches('/');
        let lookup = send_retrying(
            self.http
                .get(format!("{base}/wayback/available"))
                .query(&[("url", url.as_str())]),
            &self.retry,
        )
        .await
        .map_err(|_| "the archive could not be reached".to_string())?;
        if !lookup.status().is_success() {
            return Err(format!(
                "the archive answered HTTP {}",
                lookup.status().as_u16()
            ));
        }
        let answer: Value = lookup
            .json()
            .await
            .map_err(|_| "the archive's answer was not understood".to_string())?;
        let closest = answer
            .pointer("/archived_snapshots/closest")
            .filter(|closest| closest.get("available").and_then(Value::as_bool) == Some(true))
            .ok_or_else(|| "it has no copy of this page".to_string())?;
        let stamp = closest
            .get("timestamp")
            .and_then(Value::as_str)
            .filter(|stamp| stamp.len() == 14 && stamp.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| "its answer had no usable date".to_string())?;
        if let Some(status) = closest.get("status").and_then(Value::as_str)
            && !status.starts_with('2')
        {
            return Err(format!("its copy is of an error page (HTTP {status})"));
        }
        let date = format!("{}-{}-{}", &stamp[..4], &stamp[4..6], &stamp[6..8]);
        // `id_` asks for the page as it was captured, without the archive's own toolbar.
        let snapshot = Url::parse(&format!("{base}/web/{stamp}id_/{url}"))
            .map_err(|_| "its address for the copy was not valid".to_string())?;

        let mut current = snapshot.clone();
        let mut reply = None;
        for _ in 0..=ARCHIVE_REDIRECTS {
            let response = send_retrying(self.http.get(current.clone()), &self.retry)
                .await
                .map_err(|_| "the copy could not be read".to_string())?;
            if response.status().is_redirection() {
                let next = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|location| current.join(location).ok())
                    .filter(|next| next.origin() == snapshot.origin())
                    .ok_or_else(|| "the copy redirects away from the archive".to_string())?;
                current = next;
                continue;
            }
            reply = Some(response);
            break;
        }
        let response = reply.ok_or_else(|| "the copy redirects too often".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "the copy could not be read (HTTP {})",
                response.status().as_u16()
            ));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(content_type.is_empty()
            || content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml"))
        {
            return Err(format!("the copy is not a text page ({content_type})"));
        }
        let body = read_capped(response, ARCHIVE_PAGE_BYTES).await;
        let (title, text) = if content_type.contains("html") || body.trim_start().starts_with('<') {
            (html::title(&body), html::to_text(&body))
        } else {
            (None, body.clone())
        };
        if block::detect(200, false, &body, title.as_deref(), &text).is_some() {
            return Err("its copy is itself a block page".into());
        }
        Ok(Archived {
            page: Page {
                title,
                final_url: url.to_string(),
                text: text.chars().take(MAX_PAGE_CHARS).collect(),
            },
            date,
            url: snapshot.to_string(),
        })
    }
}
