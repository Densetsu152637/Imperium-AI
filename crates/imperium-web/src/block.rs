//! Recognises pages that are a bot-protection block instead of the page that was asked for: a
//! CAPTCHA, a Cloudflare or similar challenge, a rate limit, an access-denied wall. Their text
//! must never reach an agent as if it were the page's content.
//!
//! Detection only reads what the server sent. It looks at the status, the `cf-mitigated`
//! header, the page title and a list of well-known markers; it never solves or answers a
//! challenge.

/// How much of a page's markup is searched for markers.
const RAW_LIMIT: usize = 200_000;
/// A page with more visible text than this is not a challenge page for the phrase markers (a
/// long article about CAPTCHAs, a login page with a widget on it).
const SHORT_PHRASE_PAGE: usize = 2_500;
/// The same limit for widget markers, which also sit on ordinary forms.
const SHORT_WIDGET_PAGE: usize = 800;
/// The same limit for titles.
const SHORT_TITLE_PAGE: usize = 3_000;

/// Titles (lower case, start of the title) that challenge and block pages use.
const CHALLENGE_TITLES: [&str; 14] = [
    "just a moment",
    "attention required",
    "access denied",
    "robot check",
    "are you a robot",
    "are you human",
    "verify you are",
    "security check",
    "pardon our interruption",
    "checking your browser",
    "one more step",
    "human verification",
    "please verify",
    "ddos protection by",
];

/// Markers that only a challenge page carries, however much text it has.
const STRONG_MARKERS: [&str; 5] = [
    "cf_chl_opt",
    "cf-browser-verification",
    "__cf_chl_",
    "cf-challenge-running",
    "_cf_chl_tk",
];

/// Phrases on a challenge page's visible text (lower case).
const PHRASES: [&str; 16] = [
    "verify you are human",
    "verify you are a human",
    "checking if the site connection is secure",
    "checking your browser before accessing",
    "enable javascript and cookies to continue",
    "unusual traffic from your computer network",
    "our systems have detected unusual traffic",
    "are you a robot",
    "confirm you are a human",
    "please complete the security check",
    "sorry, you have been blocked",
    "you have been blocked",
    "access to this page has been denied",
    "pardon our interruption",
    "please turn javascript on and reload the page",
    "performance & security by cloudflare",
];

/// CAPTCHA and bot-management widgets (lower case), which small block pages embed.
const WIDGETS: [&str; 8] = [
    "g-recaptcha",
    "h-captcha",
    "hcaptcha.com/1/api.js",
    "cf-turnstile",
    "px-captcha",
    "captcha-delivery.com",
    "/recaptcha/api",
    "geo.captcha-delivery",
];

/// Why a response is a block, in words for the agent, or `None` when it looks like the page.
///
/// `status` is the HTTP status; `cf_mitigated` is whether the response carried
/// `cf-mitigated: challenge`; `raw` is the body as sent (markup included), `title` and `text`
/// the page's title and visible text.
pub fn detect(
    status: u16,
    cf_mitigated: bool,
    raw: &str,
    title: Option<&str>,
    text: &str,
) -> Option<String> {
    if cf_mitigated {
        return Some("a Cloudflare challenge (cf-mitigated: challenge)".into());
    }
    if status == 429 {
        return Some("rate limited (HTTP 429 Too Many Requests)".into());
    }
    if let Some(what) = body_signal(raw, title, text) {
        return Some(if status >= 400 {
            format!("{what}, HTTP {status}")
        } else {
            what
        });
    }
    // A bare 403 is an ordinary refusal (a login wall, a permission), not a challenge.
    None
}

fn body_signal(raw: &str, title: Option<&str>, text: &str) -> Option<String> {
    let visible = text.chars().count();
    if visible < SHORT_TITLE_PAGE
        && let Some(title) = title
    {
        let lower = title.trim().to_ascii_lowercase();
        if CHALLENGE_TITLES
            .iter()
            .any(|start| lower.starts_with(start))
        {
            return Some(format!("a block page titled {:?}", title.trim()));
        }
    }
    let raw: String = raw.chars().take(RAW_LIMIT).collect();
    let raw = raw.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();
    let find = |markers: &[&'static str]| -> Option<&'static str> {
        markers
            .iter()
            .find(|marker| raw.contains(**marker) || text.contains(**marker))
            .copied()
    };
    if let Some(marker) = find(&STRONG_MARKERS) {
        return Some(describe(marker));
    }
    if visible < SHORT_PHRASE_PAGE
        && let Some(marker) = find(&PHRASES)
    {
        return Some(describe(marker));
    }
    if visible < SHORT_WIDGET_PAGE
        && let Some(marker) = find(&WIDGETS)
    {
        return Some(describe(marker));
    }
    None
}

fn describe(marker: &str) -> String {
    let human = ["captcha", "human", "robot", "turnstile"]
        .iter()
        .any(|word| marker.contains(word));
    if human {
        "a CAPTCHA or human-verification page".into()
    } else {
        "a bot-protection challenge page".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    /// Runs `detect` the way the fetcher does: title and text come from the markup.
    fn check(status: u16, markup: &str) -> Option<String> {
        detect(
            status,
            false,
            markup,
            html::title(markup).as_deref(),
            &html::to_text(markup),
        )
    }

    const CLOUDFLARE: &str = r#"<!DOCTYPE html><html lang="en-US"><head><title>Just a moment...</title>
<script>window._cf_chl_opt={cvId:'3',cZone:'example.com'};</script></head>
<body><h1>example.com</h1><p>Checking if the site connection is secure</p>
<p>Enable JavaScript and cookies to continue</p></body></html>"#;

    const RECAPTCHA: &str = r#"<html><head><title>Sign in</title></head><body>
<form><p>Please confirm.</p><div class="g-recaptcha" data-sitekey="x"></div></form>
<script src="https://www.google.com/recaptcha/api.js"></script></body></html>"#;

    const UNUSUAL_TRAFFIC: &str = r#"<html><head><title>https://example.com/search</title></head><body>
<div><b>About this page</b><p>Our systems have detected unusual traffic from your computer network.
This page checks to see if it is really you sending the requests, and not a robot.</p></div></body></html>"#;

    const AKAMAI: &str = r#"<html><head><title>Access Denied</title></head><body><h1>Access Denied</h1>
You don't have permission to access "http://example.com/" on this server.<p>Reference #18.5</p></body></html>"#;

    const TURNSTILE: &str = r#"<html><head><title>Example</title></head><body>
<p>Verify you are human by completing the action below.</p><div class="cf-turnstile"></div></body></html>"#;

    #[test]
    fn challenge_pages_are_recognised() {
        for (name, status, page) in [
            ("cloudflare", 403, CLOUDFLARE),
            ("cloudflare served as 200", 200, CLOUDFLARE),
            ("recaptcha wall", 200, RECAPTCHA),
            ("unusual traffic", 200, UNUSUAL_TRAFFIC),
            ("akamai", 403, AKAMAI),
            ("turnstile", 200, TURNSTILE),
            ("cloudflare under attack mode", 503, CLOUDFLARE),
        ] {
            let reason = check(status, page);
            assert!(reason.is_some(), "{name} was not recognised");
        }
        let reason = check(200, CLOUDFLARE).unwrap();
        assert!(reason.contains("Just a moment"), "{reason}");
        assert!(check(403, CLOUDFLARE).unwrap().contains("HTTP 403"));
    }

    #[test]
    fn status_and_header_signals_need_no_body() {
        assert!(
            detect(429, false, "", None, "")
                .unwrap()
                .contains("rate limited")
        );
        assert!(
            detect(200, true, "", None, "hello")
                .unwrap()
                .contains("Cloudflare")
        );
        // A bare 403 is not a block: only a challenge signal is.
        assert_eq!(detect(403, false, "", None, "nope"), None);
        assert_eq!(
            detect(
                403,
                false,
                "<p>Forbidden</p>",
                Some("403 Forbidden"),
                "Forbidden"
            ),
            None
        );
        // A plain server error is not a block.
        assert_eq!(
            detect(503, false, "<p>down for maintenance</p>", None, "down"),
            None
        );
        assert_eq!(detect(404, false, "", None, "Not found"), None);
    }

    #[test]
    fn ordinary_pages_are_not_blocks() {
        let article = format!(
            "<html><head><title>How CAPTCHAs work</title></head><body><p>{}</p>\
             <p>The g-recaptcha widget and the phrase verify you are human are discussed.</p></body></html>",
            "A CAPTCHA tells computers and humans apart. ".repeat(80)
        );
        assert_eq!(check(200, &article), None);
        let login = format!(
            "<html><head><title>Log in</title></head><body><p>{}</p>\
             <div class=\"g-recaptcha\"></div></body></html>",
            "Welcome back, enter your details to continue to your account. ".repeat(30)
        );
        assert_eq!(check(200, &login), None);
        assert_eq!(
            check(
                200,
                "<html><head><title>Hello</title></head><body><p>Hello, world.</p></body></html>"
            ),
            None
        );
        assert_eq!(check(200, "plain text, no markup"), None);
    }
}
