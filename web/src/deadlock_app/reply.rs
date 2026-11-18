use deadlock_protocol::PROTOCOL_VERSION;
use deadlock_protocol::paths::PROTOCOL_HEADER;
use serde::Serialize;
use serde::de::DeserializeOwned;
use topcoat::context::Cx;
use topcoat::router::content::Json;
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{HeaderMap, StatusCode};

use super::AppApiError;

pub fn json<T: Serialize>(
    cx: &Cx,
    result: Result<T, AppApiError>,
) -> topcoat::Result<Response> {
    match result {
        Ok(value) => Json(value).into_response(cx),
        Err(error) => error.respond(cx),
    }
}

pub fn no_content(
    cx: &Cx,
    result: Result<(), AppApiError>,
) -> topcoat::Result<Response> {
    match result {
        Ok(()) => (StatusCode::NO_CONTENT, ()).into_response(cx),
        Err(error) => error.respond(cx),
    }
}

pub fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, AppApiError> {
    serde_json::from_slice(body)
        .map_err(|e| AppApiError::BadRequest(format!("invalid JSON body: {e}")))
}

#[must_use]
pub fn protocol_supported(headers: &HeaderMap) -> bool {
    headers
        .get(PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| value.parse::<u32>().ok())
        == Some(PROTOCOL_VERSION)
}
