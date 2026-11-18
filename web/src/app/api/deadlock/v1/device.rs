mod token;

use deadlock_protocol::auth::{DeviceCode, DeviceSecret, DeviceStart};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::request::{Bytes, client_ip};
use topcoat::router::response::Response;
use topcoat::router::route;

use crate::auth::{db_pool, web_state};
use crate::deadlock_app::device::{
    EXPIRES_IN_SECS,
    INTERVAL_SECS,
    clean_text,
    display_user_code,
    issue,
};
use crate::deadlock_app::limits::client_key;
use crate::deadlock_app::reply::{json, parse};
use crate::deadlock_app::{AppApiError, LINK_APP_PATH};

#[route(POST)]
pub(super) async fn device_start(cx: &Cx, body: Bytes) -> Result<Response> {
    json(cx, start(cx, &body).await)
}

#[route(*)]
pub(super) async fn device_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

async fn start(
    cx: &Cx,
    body: &[u8],
) -> std::result::Result<DeviceCode, AppApiError> {
    let state = web_state(cx)?;
    state.app_limits.device_start.check(&client_key(client_ip(cx)))?;

    let start: DeviceStart = parse(body)?;
    let name = clean_text(&start.device_name).ok_or_else(|| {
        AppApiError::BadRequest(
            "device_name must be 1 to 64 bytes of text".to_owned(),
        )
    })?;
    let version = clean_text(&start.app_version).ok_or_else(|| {
        AppApiError::BadRequest(
            "app_version must be 1 to 64 bytes of text".to_owned(),
        )
    })?;

    let issued = issue(db_pool(cx)?, &name, &version).await?;

    let base = state.urls.dashboard.trim_end_matches('/');
    let verification_uri = format!("{base}{LINK_APP_PATH}");
    let user_code = display_user_code(&issued.user_code);
    Ok(DeviceCode {
        device_code: DeviceSecret::new(issued.device_code),
        verification_uri_complete: format!("{verification_uri}?code={user_code}"),
        verification_uri,
        user_code,
        expires_in_secs: EXPIRES_IN_SECS,
        interval_secs: INTERVAL_SECS,
    })
}
