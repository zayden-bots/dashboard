mod device;
mod me;
mod token;

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::request::headers;
use topcoat::router::response::Response;
use topcoat::router::{Body, Next, layer, route};

use crate::deadlock_app::AppApiError;
use crate::deadlock_app::reply::protocol_supported;

#[layer]
async fn protocol(cx: &Cx, body: Body, next: Next<'_>) -> Result<Response> {
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
