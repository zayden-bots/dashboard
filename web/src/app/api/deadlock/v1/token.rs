use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::response::Response;
use topcoat::router::route;

use crate::auth::db_pool;
use crate::deadlock_app::AppApiError;
use crate::deadlock_app::auth::authenticate;
use crate::deadlock_app::reply::no_content;
use crate::deadlock_app::tokens::revoke;

#[route(DELETE)]
pub(super) async fn revoke_token(cx: &Cx) -> Result<Response> {
    no_content(cx, revoke_caller(cx).await)
}

#[route(*)]
pub(super) async fn token_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

async fn revoke_caller(cx: &Cx) -> std::result::Result<(), AppApiError> {
    let caller = authenticate(cx).await?;
    revoke(db_pool(cx)?, caller.token_id).await?;
    Ok(())
}
