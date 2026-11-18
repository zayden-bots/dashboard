use deadlock::proxy::{Lookup, ProxyBody};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::request::uri;
use topcoat::router::response::Response;
use topcoat::router::route;

use crate::auth::web_state;
use crate::deadlock_app::AppApiError;
use crate::deadlock_app::auth::authenticate;
use crate::deadlock_app::proxy::{api_error, respond, upstream_of};

#[route(GET "./{*rest}")]
pub(super) async fn proxy(cx: &Cx) -> Result<Response> {
    match serve(cx).await {
        Ok(body) => respond(cx, body),
        Err(error) => error.respond(cx),
    }
}

#[route(* "./{*rest}")]
pub(super) async fn proxy_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

async fn serve(cx: &Cx) -> std::result::Result<ProxyBody, AppApiError> {
    let caller = authenticate(cx).await?;
    let canonical = upstream_of(uri(cx)).ok_or(AppApiError::NotProxied)?;
    let state = web_state(cx)?;

    match state.deadlock_proxy.lookup(&canonical).await.map_err(api_error)? {
        Lookup::Fresh(body) => Ok(body),
        Lookup::Miss(miss) => {
            state.app_limits.proxy_miss.check(&caller.token_id)?;
            state.deadlock_proxy.fetch(miss).await.map_err(api_error)
        },
    }
}
