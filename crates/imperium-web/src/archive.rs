//! Archived copies of pages that block plain HTTP, from the Wayback Machine's availability API.
//! The copy is always labelled as archived, with its date, by the caller.

use reqwest::header::LOCATION;
use serde_json::Value;
use url::Url;

use crate::cache::Page;
use crate::retry::send_retrying;
use crate::{ARCHIVE_PAGE_BYTES, MAX_PAGE_CHARS, Web, block, html, read_capped};

/// Redirects followed inside the archive.
const ARCHIVE_REDIRECTS: usize = 5;

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
