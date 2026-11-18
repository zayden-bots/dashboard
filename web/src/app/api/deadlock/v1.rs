mod device;
mod matches;
mod me;
mod proxy;
mod token;
mod ws;

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::request::headers;
use topcoat::router::response::Response;
use topcoat::router::{Body, Next, header, layer, route};

use crate::deadlock_app::AppApiError;
use crate::deadlock_app::reply::{is_websocket_upgrade, protocol_supported};

#[layer]
async fn protocol(cx: &Cx, body: Body, next: Next<'_>) -> Result<Response> {
    if is_websocket_upgrade(headers(cx)) && headers(cx).contains_key(header::ORIGIN)
    {
        return AppApiError::BadRequest(
            "websocket upgrades must not send an Origin header".to_owned(),
        )
        .respond(cx);
    }
    if protocol_supported(headers(cx)) {
        next.run(cx, body).await
    } else {
        AppApiError::ProtocolUnsupported.respond(cx)
    }
}

#[route(*)]
pub(super) async fn root(cx: &Cx) -> Result<Response> {
    AppApiError::NotFound.respond(cx)
}

#[route(* "./{*rest}")]
pub(super) async fn unknown(cx: &Cx) -> Result<Response> {
    AppApiError::NotFound.respond(cx)
}
