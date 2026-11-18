use std::time::Duration;

use sqlx::PgPool;
use tracing::warn;

use crate::deadlock_app::device::prune_expired;

const PRUNE_INTERVAL: Duration = Duration::from_hours(1);

pub fn spawn(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
        loop {
            ticker.tick().await;
            prune_expired_sessions(&pool).await;
            prune_expired_device_codes(&pool).await;
        }
    });
}

pub async fn prune_expired_device_codes(pool: &PgPool) {
    if let Err(e) = prune_expired(pool).await {
        warn!(?e, "failed to prune expired deadlock_device_codes");
    }
}

pub async fn prune_expired_sessions(pool: &PgPool) {
    if let Err(e) =
        sqlx::query!("DELETE FROM web_sessions WHERE expires_at <= now()")
            .execute(pool)
            .await
    {
        warn!(?e, "failed to prune expired web_sessions");
    }
}
