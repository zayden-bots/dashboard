use std::sync::Arc;

use sqlx::PgPool;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::websocket::WebSocketUpgrade;
use topcoat::router::request::FromRequest;
use topcoat::router::response::Response;
use topcoat::router::{Body, route};

use crate::auth::{db_pool, web_state};
use crate::deadlock_app::AppApiError;
use crate::deadlock_app::auth::{AppCaller, authenticate, linked_account};
use crate::deadlock_app::ws::{self, UPGRADE_LIMIT_BYTES, WsHub};

#[route(GET)]
pub(super) async fn connect(cx: &Cx, body: Body) -> Result<Response> {
    let (caller, account_id) = match admit(cx).await {
        Ok(admitted) => admitted,
        Err(e) => return e.respond(cx),
    };
    let Ok(upgrade) = WebSocketUpgrade::from_request(cx, body).await else {
        return AppApiError::BadRequest("expected a websocket upgrade".to_owned())
            .respond(cx);
    };
    let (hub, pool) = match shared(cx) {
        Ok(shared) => shared,
        Err(e) => return e.respond(cx),
    };
    upgrade
        .read_buffer_size(UPGRADE_LIMIT_BYTES)
        .write_buffer_size(UPGRADE_LIMIT_BYTES)
        .max_write_buffer_size(4 * UPGRADE_LIMIT_BYTES)
        .max_message_size(UPGRADE_LIMIT_BYTES)
        .max_frame_size(UPGRADE_LIMIT_BYTES)
        .on_upgrade(move |socket| ws::run(socket, hub, pool, caller, account_id))
}

#[route(*)]
pub(super) async fn connect_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

fn shared(cx: &Cx) -> std::result::Result<(Arc<WsHub>, PgPool), AppApiError> {
    Ok((Arc::clone(&web_state(cx)?.app_ws), db_pool(cx)?.clone()))
}

async fn admit(cx: &Cx) -> std::result::Result<(AppCaller, u32), AppApiError> {
    let caller = authenticate(cx).await?;
    let account_id = linked_account(db_pool(cx)?, caller.discord_id)
        .await?
        .ok_or(AppApiError::NotLinked)?;
    web_state(cx)?.app_limits.ws_upgrade.check(&caller.token_id)?;
    Ok((caller, account_id))
}
