use deadlock_protocol::ws::{CLOSE_ACCOUNT_MISMATCH, CLOSE_UNAUTHORIZED};
use sqlx::PgPool;

use super::hub::SocketInfo;
use crate::deadlock_app::{AppApiError, auth, tokens};

pub async fn check(
    pool: &PgPool,
    info: &SocketInfo,
) -> Result<Option<u16>, AppApiError> {
    if !tokens::is_live(pool, info.token_id).await? {
        return Ok(Some(CLOSE_UNAUTHORIZED));
    }
    if auth::linked_account(pool, info.discord_id).await? != Some(info.account_id) {
        return Ok(Some(CLOSE_ACCOUNT_MISMATCH));
    }
    Ok(None)
}
