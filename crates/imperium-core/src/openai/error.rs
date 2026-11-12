//! Errors in the shape OpenAI clients parse: `{"error": {"message", "type", "param", "code"}}`.

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub(super) struct ApiError {
    status: StatusCode,
    /// OpenAI's `type`: `invalid_request_error`, `server_error`, and so on.
    kind: &'static str,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, kind: &'static str, code: &'static str, message: String) -> Self {
        ApiError {
            status,
            kind,
            code,
            message,
        }
    }

    pub(super) fn invalid(message: impl Into<String>) -> Self {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid_request",
            message.into(),
        )
    }

    pub(super) fn unauthorized() -> Self {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            "invalid_api_key",
            "Incorrect API key provided. Send the API token from Imperium (Settings → API) as \
             `Authorization: Bearer <token>`."
                .into(),
        )
    }

    pub(super) fn model_not_found(model: &str, hint: &str) -> Self {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "model_not_found",
            format!("The model `{model}` is not served by this API. {hint}"),
        )
    }

    pub(super) fn no_route(path: &str) -> Self {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "unknown_url",
            format!(
                "Unknown request URL: {path}. This API serves /v1/models and /v1/chat/completions."
            ),
        )
    }

    /// The API already has all the calls in flight it allows.
    pub(super) fn too_many_calls() -> Self {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
            "rate_limit_exceeded",
            "The API is serving as many calls at once as it allows \
             (api.openai_compatible.max_concurrent). Try again when one finishes."
                .into(),
        )
    }

    /// A used-up budget refuses the call. OpenAI reports exhausted quotas as 429
    /// `insufficient_quota`, which clients know not to retry at once.
    pub(super) fn budget(message: impl Into<String>) -> Self {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "insufficient_quota",
            "budget_exceeded",
            message.into(),
        )
    }

    /// The model cannot be reached or loaded right now.
    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "model_unavailable",
            message.into(),
        )
    }

    /// The model's server (or the hosted endpoint) answered with a failure.
    pub(super) fn upstream(status: u16, detail: &str) -> Self {
        match status {
            400 | 413 | 422 => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "rejected_by_model_server",
                detail.into(),
            ),
            429 => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_exceeded",
                "rate_limit_exceeded",
                detail.into(),
            ),
            401 | 403 => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "server_error",
                "upstream_auth_failed",
                format!("The model's endpoint refused Imperium's credentials: {detail}"),
            ),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "server_error",
                "upstream_error",
                format!("The model's endpoint answered HTTP {status}: {detail}"),
            ),
        }
    }

    pub(super) fn unreachable(message: impl Into<String>) -> Self {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "server_error",
            "upstream_unreachable",
            message.into(),
        )
    }

    pub(super) fn shutting_down() -> Self {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "shutting_down",
            "imperium-core is shutting down".into(),
        )
    }

    /// The error as a JSON value, for a failure after a stream has begun.
    pub(super) fn to_json(&self) -> serde_json::Value {
        json!({"error": {
            "message": self.message,
            "type": self.kind,
            "param": null,
            "code": self.code,
        }})
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.to_json())).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                "Bearer".parse().expect("a header"),
            );
        }
        response
    }
}
