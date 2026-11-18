use deadlock::ProxyError;
use deadlock::proxy::ProxyBody;
use deadlock_protocol::paths::{
    FETCHED_AT_HEADER,
    PROXY,
    STALE_HEADER,
    proxy_upstream,
};
use topcoat::context::Cx;
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};

use super::AppApiError;

#[must_use]
pub fn upstream_of(uri: &Uri) -> Option<String> {
    let rest = uri.path().strip_prefix(PROXY)?;
    let raw = uri
        .query()
        .map_or_else(|| rest.to_owned(), |query| format!("{rest}?{query}"));
    proxy_upstream(&raw)
}

#[must_use]
pub fn api_error(error: ProxyError) -> AppApiError {
    match error {
        ProxyError::NotProxied(_) => AppApiError::NotProxied,
        ProxyError::NotFound => {
            tracing::debug!("deadlock proxy upstream has no such resource");
            AppApiError::UpstreamNotFound
        },
        ProxyError::Rejected { status } => {
            tracing::warn!(status, "deadlock proxy upstream rejected the request");
            AppApiError::BadRequest(format!(
                "deadlock-api rejected the request (status {status})"
            ))
        },
        ProxyError::Upstream(_)
        | ProxyError::InvalidBody(_)
        | ProxyError::Busy
        | ProxyError::TooLarge { .. } => {
            tracing::warn!(error = %error, "deadlock proxy upstream failed");
            AppApiError::Upstream
        },
        ProxyError::Sqlx(e) => AppApiError::Database(e),
    }
}

pub fn respond(cx: &Cx, body: ProxyBody) -> topcoat::Result<Response> {
    let mut headers = HeaderMap::new();
    headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static(FETCHED_AT_HEADER),
        HeaderValue::from(body.fetched_at.as_millisecond()),
    );
    if body.stale {
        headers.insert(
            HeaderName::from_static(STALE_HEADER),
            HeaderValue::from_static("1"),
        );
    }
    (StatusCode::OK, headers, body.body).into_response(cx)
}
