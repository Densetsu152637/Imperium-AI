//! Polite retries: a rate limit (429) or a server error (500, 502, 503, 504) is tried again
//! after a pause, a few times at most. The pause is the server's `Retry-After` when it gave one
//! (in seconds) and a doubling backoff otherwise; a server that asks for longer than the cap is
//! not waited for, its answer is returned as it is.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{RequestBuilder, Response, StatusCode};

/// How often and how long a request is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt; 0 never retries.
    pub max_retries: u32,
    /// The pause before the first retry when the server named none; it doubles each time.
    pub base: Duration,
    /// The longest pause. A `Retry-After` above it ends the retrying.
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 2,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// The pause before retry number `attempt` (0 for the first), or `None` to stop retrying.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Option<Duration> {
        match retry_after {
            Some(wait) if wait > self.cap => None,
            Some(wait) => Some(wait),
            None => Some(
                self.base
                    .saturating_mul(2u32.saturating_pow(attempt))
                    .min(self.cap),
            ),
        }
    }
}

/// Whether a response with `status` is worth asking again.
pub fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || matches!(status.as_u16(), 500 | 502 | 503 | 504)
}

/// The pause a `Retry-After` header asks for. Only the seconds form is read; a date or anything
/// else counts as not given, so the backoff applies.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    value.parse::<u64>().ok().map(Duration::from_secs)
}

/// Sends `request`, asking again after a pause while the answer is a 429 or a 5xx from the list
/// above and the policy allows. Returns the last response, retried or not. A request whose body
/// cannot be cloned is sent once.
pub async fn send_retrying(
    request: RequestBuilder,
    policy: &RetryPolicy,
) -> Result<Response, reqwest::Error> {
    let mut attempt = 0;
    loop {
        let Some(this_try) = request.try_clone() else {
            return request.send().await;
        };
        let response = this_try.send().await?;
        if attempt >= policy.max_retries || !is_retryable(response.status()) {
            return Ok(response);
        }
        let Some(wait) = policy.delay(attempt, retry_after(response.headers())) else {
            return Ok(response);
        };
        drop(response);
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::StatusCode as Status;
    use axum::routing::get;

    use super::*;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        url
    }

    /// Fails `failures` times with `status` (and `retry_after` when given), then answers 200.
    fn flaky(
        hits: &Arc<AtomicUsize>,
        failures: usize,
        status: Status,
        retry_after: Option<&'static str>,
    ) -> axum::routing::MethodRouter {
        let hits = hits.clone();
        get(move || {
            let hits = hits.clone();
            async move {
                let seen = hits.fetch_add(1, Ordering::SeqCst);
                let mut response = axum::response::Response::builder();
                if seen < failures {
                    response = response.status(status);
                    if let Some(wait) = retry_after {
                        response = response.header("retry-after", wait);
                    }
                }
                response.body(axum::body::Body::from("body")).unwrap()
            }
        })
    }

    fn quick() -> RetryPolicy {
        RetryPolicy {
            max_retries: 2,
            base: Duration::from_millis(1),
            cap: Duration::from_millis(50),
        }
    }

    #[test]
    fn the_server_names_the_pause_or_the_backoff_doubles_to_the_cap() {
        let policy = RetryPolicy {
            max_retries: 3,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(1),
        };
        let secs = Duration::from_secs;
        assert_eq!(policy.delay(0, Some(secs(0))), Some(secs(0)));
        assert_eq!(policy.delay(1, Some(secs(1))), Some(secs(1)));
        assert_eq!(
            policy.delay(0, Some(secs(2))),
            None,
            "over the cap: give up"
        );
        assert_eq!(policy.delay(0, None), Some(Duration::from_millis(100)));
        assert_eq!(policy.delay(2, None), Some(Duration::from_millis(400)));
        assert_eq!(policy.delay(9, None), Some(secs(1)), "never above the cap");
    }

    #[test]
    fn retry_after_reads_seconds_only() {
        let headers = |value: &str| {
            let mut map = HeaderMap::new();
            map.insert(RETRY_AFTER, value.parse().unwrap());
            map
        };
        assert_eq!(retry_after(&headers("7")), Some(Duration::from_secs(7)));
        assert_eq!(retry_after(&headers(" 0 ")), Some(Duration::ZERO));
        assert_eq!(retry_after(&headers("Wed, 21 Oct 2015 07:28:00 GMT")), None);
        assert_eq!(retry_after(&HeaderMap::new()), None);
        assert!(is_retryable(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable(StatusCode::BAD_GATEWAY));
        assert!(!is_retryable(StatusCode::FORBIDDEN));
        assert!(!is_retryable(StatusCode::NOT_FOUND));
        assert!(!is_retryable(StatusCode::NOT_IMPLEMENTED));
    }

    #[tokio::test]
    async fn a_rate_limit_is_retried_after_the_pause_the_server_named() {
        let hits = Arc::new(AtomicUsize::new(0));
        let base =
            serve(Router::new().route("/", flaky(&hits, 2, Status::TOO_MANY_REQUESTS, Some("0"))))
                .await;
        let client = reqwest::Client::new();
        let response = send_retrying(client.get(&base), &quick()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_pause_over_the_cap_is_not_waited_for() {
        let hits = Arc::new(AtomicUsize::new(0));
        let base = serve(Router::new().route(
            "/",
            flaky(&hits, 10, Status::TOO_MANY_REQUESTS, Some("3600")),
        ))
        .await;
        let started = std::time::Instant::now();
        let response = send_retrying(reqwest::Client::new().get(&base), &quick())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "no retry");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn server_errors_back_off_and_stop_at_the_cap_on_attempts() {
        let hits = Arc::new(AtomicUsize::new(0));
        let base =
            serve(Router::new().route("/", flaky(&hits, 100, Status::SERVICE_UNAVAILABLE, None)))
                .await;
        let response = send_retrying(reqwest::Client::new().get(&base), &quick())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "one attempt and two retries"
        );
    }

    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let hits = Arc::new(AtomicUsize::new(0));
        let base =
            serve(Router::new().route("/", flaky(&hits, 100, Status::FORBIDDEN, None))).await;
        let response = send_retrying(reqwest::Client::new().get(&base), &quick())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
