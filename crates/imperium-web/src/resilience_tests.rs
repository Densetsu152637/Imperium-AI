//! Blocks, fallbacks and retries of `web_fetch` and `web_search`, against local test servers
//! only (never the internet).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{MethodRouter, get};
use serde_json::{Value, json};

use crate::retry::RetryPolicy;
use crate::{
    FetchOptions, Hit, PageRenderer, RenderFuture, RenderedPage, SearchFuture, SearchProvider,
    Searxng, Web, WebError, render_outcome,
};

const CLOUDFLARE: &str = r#"<!DOCTYPE html><html lang="en-US"><head><title>Just a moment...</title>
<script>window._cf_chl_opt={cvId:'3',cZone:'example.com'};</script></head>
<body><h1>example.com</h1><p>Checking if the site connection is secure</p>
<p>Enable JavaScript and cookies to continue</p></body></html>"#;

const FORBIDDEN: &str =
    "<html><head><title>403</title></head><body>blocked-body-text</body></html>";
const REAL: &str =
    "<html><head><title>Real page</title></head><body><p>Real page text.</p></body></html>";
const ARCHIVED: &str =
    "<html><head><title>Old page</title></head><body><p>Archived body text.</p></body></html>";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn quick() -> RetryPolicy {
    RetryPolicy {
        max_retries: 2,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(50),
    }
}

fn count(hits: &Arc<AtomicUsize>) -> usize {
    hits.load(Ordering::SeqCst)
}

/// A page route that counts its requests and answers with `status` and `body`.
fn page(hits: &Arc<AtomicUsize>, status: StatusCode, body: &'static str) -> MethodRouter {
    let hits = hits.clone();
    get(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            (status, [("content-type", "text/html")], body)
        }
    })
}

/// A route answering `status` with a `Retry-After`, counting its requests.
fn limited(hits: &Arc<AtomicUsize>, status: StatusCode, retry_after: &'static str) -> MethodRouter {
    let hits = hits.clone();
    get(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            (
                status,
                [("content-type", "text/html"), ("retry-after", retry_after)],
                "<html><body>slow down</body></html>",
            )
        }
    })
}

/// The Wayback Machine's availability API and captures, as one test server's routes.
fn archive_routes(
    router: Router,
    answer: Value,
    capture: &'static str,
    asked: &Arc<AtomicUsize>,
) -> Router {
    let asked = asked.clone();
    router
        .route(
            "/wayback/available",
            get(move || {
                let asked = asked.clone();
                let answer = answer.clone();
                async move {
                    asked.fetch_add(1, Ordering::SeqCst);
                    axum::Json(answer)
                }
            }),
        )
        .route(
            "/web/{*rest}",
            get(move || async move { ([("content-type", "text/html")], capture) }),
        )
}

fn closest(stamp: &str) -> Value {
    json!({"archived_snapshots": {"closest": {
        "available": true, "status": "200", "timestamp": stamp,
        "url": format!("http://web.archive.org/web/{stamp}/https://example.com/")
    }}})
}

fn web_with_archive(base: &str) -> Web {
    Web::with_options(
        None,
        FetchOptions {
            archive_base: Some(base.to_string()),
            ..FetchOptions::default()
        },
    )
    .with_retry(quick())
}

/// A browser stand-in that records how often it was asked.
struct FakeBrowser {
    calls: Arc<AtomicUsize>,
    answer: Result<RenderedPage, String>,
}

impl PageRenderer for FakeBrowser {
    fn render<'a>(&'a self, _url: &'a str) -> RenderFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

fn browser(calls: &Arc<AtomicUsize>, answer: Result<RenderedPage, String>) -> FakeBrowser {
    FakeBrowser {
        calls: calls.clone(),
        answer,
    }
}

fn rendered(title: &str, text: &str) -> Result<RenderedPage, String> {
    Ok(RenderedPage {
        title: Some(title.to_string()),
        final_url: "https://example.com/".into(),
        text: text.to_string(),
    })
}

#[tokio::test]
async fn block_pages_are_reported_and_never_returned_or_cached() {
    let challenged = Arc::new(AtomicUsize::new(0));
    let forbidden = Arc::new(AtomicUsize::new(0));
    let missing = Arc::new(AtomicUsize::new(0));
    let base = serve(
        Router::new()
            .route("/challenge", page(&challenged, StatusCode::OK, CLOUDFLARE))
            .route(
                "/forbidden",
                page(&forbidden, StatusCode::FORBIDDEN, FORBIDDEN),
            )
            .route(
                "/missing",
                page(&missing, StatusCode::NOT_FOUND, "<p>gone</p>"),
            ),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let web = Web::with_options(
        None,
        FetchOptions {
            cache_dir: Some(dir.path().to_path_buf()),
            ..FetchOptions::default()
        },
    )
    .with_retry(quick());

    for path in ["challenge", "forbidden"] {
        let error = web
            .fetch(&format!("{base}/{path}"), 0, 0)
            .await
            .unwrap_err();
        assert!(matches!(error, WebError::Blocked { .. }), "{path}: {error}");
        let message = error.to_string();
        assert!(message.contains("blocked by bot protection"), "{message}");
        assert!(!message.contains("Checking if the site"), "{message}");
        assert!(!message.contains("blocked-body-text"), "{message}");
    }
    // A block is not cached: the page is asked for again.
    let _ = web.fetch(&format!("{base}/challenge"), 0, 0).await;
    assert_eq!(count(&challenged), 2);
    // An ordinary error stays an ordinary error.
    let error = web
        .fetch(&format!("{base}/missing"), 0, 0)
        .await
        .unwrap_err();
    assert!(matches!(error, WebError::Http(_)), "{error}");
    assert!(error.to_string().contains("404"), "{error}");
}

#[tokio::test]
async fn a_rate_limit_is_retried_politely_and_a_long_wait_is_reported() {
    let flaky = Arc::new(AtomicUsize::new(0));
    let long = Arc::new(AtomicUsize::new(0));
    let flaky_hits = flaky.clone();
    let base = serve(
        Router::new()
            .route(
                "/flaky",
                get(move || {
                    let hits = flaky_hits.clone();
                    async move {
                        let seen = hits.fetch_add(1, Ordering::SeqCst);
                        if seen < 2 {
                            (
                                StatusCode::TOO_MANY_REQUESTS,
                                [("content-type", "text/html"), ("retry-after", "0")],
                                "<p>wait</p>",
                            )
                        } else {
                            (
                                StatusCode::OK,
                                [("content-type", "text/html"), ("retry-after", "0")],
                                REAL,
                            )
                        }
                    }
                }),
            )
            .route(
                "/long",
                limited(&long, StatusCode::TOO_MANY_REQUESTS, "3600"),
            ),
    )
    .await;
    let web = Web::new(None).with_retry(quick());
    let text = web.fetch(&format!("{base}/flaky"), 0, 0).await.unwrap();
    assert!(text.contains("Real page text."), "{text}");
    assert_eq!(count(&flaky), 3, "two retries, then the page");

    let error = web.fetch(&format!("{base}/long"), 0, 0).await.unwrap_err();
    assert_eq!(count(&long), 1, "a wait over the cap is not waited for");
    let message = error.to_string();
    assert!(message.contains("rate limited"), "{message}");
    assert!(message.contains("asked to wait 3600 s"), "{message}");
}

#[tokio::test]
async fn requests_carry_an_honest_stable_user_agent() {
    let base = serve(Router::new().route(
        "/ua",
        get(|headers: axum::http::HeaderMap| async move {
            let agent = headers
                .get("user-agent")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            ([("content-type", "text/plain")], agent)
        }),
    ))
    .await;
    let text = Web::new(None)
        .fetch(&format!("{base}/ua"), 0, 0)
        .await
        .unwrap();
    assert!(text.ends_with(crate::USER_AGENT), "{text}");
    assert!(crate::USER_AGENT.starts_with("Imperium/"));
}

#[tokio::test]
async fn blocked_pages_are_tried_in_the_browser_then_in_the_archive() {
    let blocked = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(AtomicUsize::new(0));
    let base = serve(archive_routes(
        Router::new()
            .route("/open", page(&blocked, StatusCode::OK, REAL))
            .route("/walled", page(&blocked, StatusCode::FORBIDDEN, FORBIDDEN)),
        closest("20230405060708"),
        ARCHIVED,
        &asked,
    ))
    .await;
    let web = web_with_archive(&base);
    let opened = Arc::new(AtomicUsize::new(0));

    // A page that loads is read directly: neither fallback is asked.
    let fine = browser(&opened, rendered("Unused", "unused"));
    let text = web
        .fetch_resilient(&format!("{base}/open"), 0, 0, &|_| true, Some(&fine))
        .await
        .unwrap();
    assert!(text.contains("Real page text."), "{text}");
    assert_eq!((count(&opened), count(&asked)), (0, 0));

    // Blocked: the browser comes first, and the archive is not asked when it works.
    let works = browser(&opened, rendered("Rendered", "Rendered body"));
    let text = web
        .fetch_resilient(&format!("{base}/walled"), 0, 0, &|_| true, Some(&works))
        .await
        .unwrap();
    assert!(text.starts_with("Read through the browser"), "{text}");
    assert!(text.contains("Rendered body"), "{text}");
    assert_eq!((count(&opened), count(&asked)), (1, 0));

    // The browser is blocked too: the archived copy, labelled and dated.
    let walled = browser(
        &opened,
        rendered("Just a moment...", "Verify you are human"),
    );
    let text = web
        .fetch_resilient(&format!("{base}/walled"), 0, 0, &|_| true, Some(&walled))
        .await
        .unwrap();
    assert_eq!(
        (count(&opened), count(&asked)),
        (2, 1),
        "browser, then archive"
    );
    assert!(
        text.starts_with("ARCHIVED COPY, not the live page"),
        "{text}"
    );
    assert!(text.contains("captured it on 2023-04-05"), "{text}");
    assert!(text.contains("/web/20230405060708id_/"), "{text}");
    assert!(text.contains("Archived body text."), "{text}");
    assert!(!text.contains("Verify you are human"), "{text}");

    // The browser could not run at all: straight to the archive.
    let broken = browser(&opened, Err("no browser is installed".into()));
    let text = web
        .fetch_resilient(&format!("{base}/walled"), 0, 0, &|_| true, Some(&broken))
        .await
        .unwrap();
    assert!(text.contains("ARCHIVED COPY"), "{text}");

    // With no browser at all the archive still answers.
    let text = web.fetch(&format!("{base}/walled"), 0, 0).await.unwrap();
    assert!(text.contains("ARCHIVED COPY"), "{text}");
}

#[tokio::test]
async fn when_nothing_works_the_result_is_blocked_with_what_was_tried() {
    let hits = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(AtomicUsize::new(0));
    // The archive has no copy.
    let base = serve(archive_routes(
        Router::new().route("/walled", page(&hits, StatusCode::FORBIDDEN, FORBIDDEN)),
        json!({"archived_snapshots": {}}),
        ARCHIVED,
        &asked,
    ))
    .await;
    let opened = Arc::new(AtomicUsize::new(0));
    let broken = browser(&opened, Err("no browser is installed".into()));
    let error = web_with_archive(&base)
        .fetch_resilient(&format!("{base}/walled"), 0, 0, &|_| true, Some(&broken))
        .await
        .unwrap_err();
    assert!(matches!(error, WebError::Blocked { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("no browser is installed"), "{message}");
    assert!(message.contains("no copy of this page"), "{message}");
    assert!(message.contains("blocked by bot protection"), "{message}");
    assert!(!message.contains("blocked-body-text"), "{message}");

    // The archive's copy is itself a challenge page.
    let walled = Arc::new(AtomicUsize::new(0));
    let again = Arc::new(AtomicUsize::new(0));
    let base = serve(archive_routes(
        Router::new().route("/walled", page(&walled, StatusCode::FORBIDDEN, FORBIDDEN)),
        closest("20200101000000"),
        CLOUDFLARE,
        &again,
    ))
    .await;
    let error = web_with_archive(&base)
        .fetch(&format!("{base}/walled"), 0, 0)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("itself a block page"), "{error}");
}

#[tokio::test]
async fn archives_are_not_asked_when_off_or_for_urls_with_credentials() {
    let hits = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(AtomicUsize::new(0));
    let base = serve(archive_routes(
        Router::new().route("/walled", page(&hits, StatusCode::FORBIDDEN, FORBIDDEN)),
        closest("20230405060708"),
        ARCHIVED,
        &asked,
    ))
    .await;
    // Off by default (`archive_base` is `None`).
    let error = Web::new(None)
        .fetch(&format!("{base}/walled"), 0, 0)
        .await
        .unwrap_err();
    assert!(matches!(error, WebError::Blocked { .. }), "{error}");
    assert_eq!(count(&asked), 0);
    // A URL carrying credentials never leaves for a third party.
    let with_login = base.replace("http://", "http://user:pw@");
    let error = web_with_archive(&base)
        .fetch(&format!("{with_login}/walled"), 0, 0)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("carries credentials"), "{error}");
    assert_eq!(count(&asked), 0);
}

/// What a scripted search provider answers.
#[derive(Debug, Clone, Copy)]
enum Mode {
    Hits,
    Empty,
    Blocked,
    Broken,
}

#[derive(Debug)]
struct Scripted {
    name: &'static str,
    mode: Mode,
    calls: Arc<Mutex<Vec<String>>>,
}

impl SearchProvider for Scripted {
    fn search<'a>(
        &'a self,
        _http: &'a reqwest::Client,
        _query: &'a str,
        _count: usize,
    ) -> SearchFuture<'a> {
        self.calls.lock().unwrap().push(self.name.to_string());
        let answer = match self.mode {
            Mode::Hits => Ok(vec![Hit {
                title: format!("hit from {}", self.name),
                url: "https://example.com/a".into(),
                snippet: String::new(),
            }]),
            Mode::Empty => Ok(Vec::new()),
            Mode::Blocked => Err(WebError::Blocked {
                url: "api.example".into(),
                reason: "rate limited (HTTP 429 Too Many Requests)".into(),
                tried: Vec::new(),
            }),
            Mode::Broken => Err(WebError::Http("HTTP 500 from api.example".into())),
        };
        Box::pin(async move { answer })
    }
}

fn chain(steps: &[(&'static str, Mode)], calls: &Arc<Mutex<Vec<String>>>) -> Web {
    let providers = steps
        .iter()
        .map(|(name, mode)| {
            let provider = Scripted {
                name,
                mode: *mode,
                calls: calls.clone(),
            };
            (
                name.to_string(),
                Arc::new(provider) as Arc<dyn SearchProvider>,
            )
        })
        .collect();
    Web::with_chain(providers, FetchOptions::default())
}

fn order(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    calls.lock().unwrap().clone()
}

#[tokio::test]
async fn search_falls_through_the_chain_and_says_who_answered() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let web = chain(
        &[
            ("brave", Mode::Blocked),
            ("tavily", Mode::Empty),
            ("kagi", Mode::Broken),
            ("searxng", Mode::Hits),
            ("exa", Mode::Hits),
        ],
        &calls,
    );
    let outcome = web.search_chain("rust", 5).await.unwrap();
    assert_eq!(outcome.provider, "searxng");
    assert_eq!(outcome.hits.len(), 1);
    assert_eq!(
        order(&calls),
        ["brave", "tavily", "kagi", "searxng"],
        "exa is not asked"
    );
    let shown = render_outcome(&outcome);
    assert!(
        shown.contains("Answered by search provider: searxng"),
        "{shown}"
    );
    assert!(shown.contains("Skipped brave: "), "{shown}");
    assert!(shown.contains("blocked by bot protection"), "{shown}");
    assert!(shown.contains("Skipped tavily: no results"), "{shown}");
    assert!(
        shown.contains("Skipped kagi: HTTP 500 from api.example"),
        "{shown}"
    );

    // The first provider answering ends the chain.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let web = chain(&[("brave", Mode::Hits), ("tavily", Mode::Hits)], &calls);
    assert_eq!(web.search_chain("rust", 5).await.unwrap().provider, "brave");
    assert_eq!(order(&calls), ["brave"]);
}

#[tokio::test]
async fn search_reports_every_failure_or_an_empty_answer() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let web = chain(
        &[("brave", Mode::Blocked), ("tavily", Mode::Broken)],
        &calls,
    );
    let message = web.search_chain("rust", 5).await.unwrap_err().to_string();
    assert!(message.contains("failed on every provider"), "{message}");
    assert!(message.contains("brave: "), "{message}");
    assert!(message.contains("tavily: HTTP 500"), "{message}");

    // Some failed and one found nothing: an empty answer, not an error.
    let web = chain(&[("brave", Mode::Blocked), ("tavily", Mode::Empty)], &calls);
    let outcome = web.search_chain("rust", 5).await.unwrap();
    assert!(outcome.hits.is_empty());
    assert_eq!(outcome.provider, "tavily");
    assert!(render_outcome(&outcome).starts_with("No results."));

    // A single provider's error is passed on as it is.
    let web = chain(&[("brave", Mode::Broken)], &calls);
    assert_eq!(
        web.search_chain("rust", 5).await.unwrap_err().to_string(),
        "HTTP 500 from api.example"
    );
    // And a single provider adds no provider line.
    let web = chain(&[("brave", Mode::Hits)], &calls);
    let shown = render_outcome(&web.search_chain("rust", 5).await.unwrap());
    assert!(!shown.contains("Answered by"), "{shown}");
    assert!(matches!(
        Web::with_chain(Vec::new(), FetchOptions::default())
            .search_chain("rust", 5)
            .await,
        Err(WebError::NoProvider)
    ));
}

#[tokio::test]
async fn a_search_engine_behind_bot_protection_is_blocked_and_the_next_provider_answers() {
    let challenged = Arc::new(AtomicUsize::new(0));
    let limited_hits = Arc::new(AtomicUsize::new(0));
    let blocked_base = serve(Router::new().route(
        "/search",
        page(&challenged, StatusCode::FORBIDDEN, CLOUDFLARE),
    ))
    .await;
    let html_base =
        serve(Router::new().route("/search", page(&challenged, StatusCode::OK, CLOUDFLARE))).await;
    let limit_base = serve(Router::new().route(
        "/search",
        limited(&limited_hits, StatusCode::TOO_MANY_REQUESTS, "3600"),
    ))
    .await;
    let good_base = serve(Router::new().route(
        "/search",
        get(|| async {
            axum::Json(json!({"results": [
                {"title": "Tokio", "url": "https://tokio.rs", "content": "Async runtime"}
            ]}))
        }),
    ))
    .await;

    for (name, base) in [
        ("403 challenge", &blocked_base),
        ("200 challenge instead of JSON", &html_base),
        ("429 with a long wait", &limit_base),
    ] {
        let alone = Web::new(Some(Arc::new(Searxng { url: base.clone() })));
        let error = alone.search("tokio", 3).await.unwrap_err();
        assert!(matches!(error, WebError::Blocked { .. }), "{name}: {error}");
        assert!(
            !error.to_string().contains("Checking if the site"),
            "{name}"
        );
    }
    assert_eq!(count(&limited_hits), 1, "a 3600 s wait is not waited for");

    let providers: Vec<(String, Arc<dyn SearchProvider>)> = vec![
        (
            "blocked".into(),
            Arc::new(Searxng { url: blocked_base }) as Arc<dyn SearchProvider>,
        ),
        (
            "limited".into(),
            Arc::new(Searxng { url: limit_base }) as Arc<dyn SearchProvider>,
        ),
        (
            "self-hosted".into(),
            Arc::new(Searxng { url: good_base }) as Arc<dyn SearchProvider>,
        ),
    ];
    let web = Web::with_chain(providers, FetchOptions::default());
    let outcome = web.search_chain("tokio", 3).await.unwrap();
    assert_eq!(outcome.provider, "self-hosted");
    assert_eq!(outcome.hits[0].url, "https://tokio.rs");
    assert_eq!(outcome.notes.len(), 2, "{:?}", outcome.notes);
    assert!(
        outcome.notes[0].starts_with("blocked: "),
        "{:?}",
        outcome.notes
    );
    assert!(
        outcome.notes[1].contains("rate limited"),
        "{:?}",
        outcome.notes
    );
}

#[tokio::test]
async fn a_provider_rate_limit_is_retried_after_retry_after() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let base = serve(Router::new().route(
        "/search",
        get(move || {
            let hits = seen.clone();
            async move {
                if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("content-type", "application/json"), ("retry-after", "0")],
                        "{}".to_string(),
                    )
                } else {
                    (
                        StatusCode::OK,
                        [("content-type", "application/json"), ("retry-after", "0")],
                        json!({"results": [{"title": "T", "url": "https://t.example", "content": "C"}]})
                            .to_string(),
                    )
                }
            }
        }),
    ))
    .await;
    let hits_found = Web::new(Some(Arc::new(Searxng { url: base })))
        .search("t", 3)
        .await
        .unwrap();
    assert_eq!(hits_found.len(), 1);
    assert_eq!(count(&hits), 2);
}
