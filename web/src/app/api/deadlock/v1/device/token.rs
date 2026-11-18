use deadlock_protocol::auth::{DevicePoll, DevicePollRequest};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::request::Bytes;
use topcoat::router::response::Response;
use topcoat::router::route;

use crate::auth::db_pool;
use crate::deadlock_app::AppApiError;
use crate::deadlock_app::device::poll;
use crate::deadlock_app::reply::{json, parse};

#[route(POST)]
pub(super) async fn device_token(cx: &Cx, body: Bytes) -> Result<Response> {
    json(cx, check(cx, &body).await)
}

#[route(*)]
pub(super) async fn device_token_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

async fn check(
    cx: &Cx,
    body: &[u8],
) -> std::result::Result<DevicePoll, AppApiError> {
    let request: DevicePollRequest = parse(body)?;
    poll(db_pool(cx)?, request.device_code.expose()).await
}
