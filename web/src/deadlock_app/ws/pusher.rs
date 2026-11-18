use std::sync::Arc;
use std::time::Duration;

use deadlock_protocol::ws::{CLOSE_ACCOUNT_MISMATCH, CLOSE_UNAUTHORIZED, Down};
use sqlx::PgPool;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::{self};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::warn;
use zayden_app::events::{AppEvent, DeadlockAppEvent};

use super::hub::WsHub;
use super::verify;
use crate::deadlock_app::{AppApiError, auth};

const REVALIDATE_EVERY: Duration = Duration::from_secs(300);

pub fn spawn_pusher(
    hub: Arc<WsHub>,
    pool: PgPool,
    mut events: broadcast::Receiver<AppEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut revalidate = tokio::time::interval_at(
            Instant::now() + REVALIDATE_EVERY,
            REVALIDATE_EVERY,
        );
        revalidate.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                received = events.recv() => match received {
                    Ok(AppEvent::DeadlockApp(event)) => {
                        if let Err(e) = apply(&hub, &pool, event).await {
                            warn!(error = %e, "deadlock: websocket push failed");
                        }
                    },
                    Ok(AppEvent::Resync) | Err(RecvError::Lagged(_)) => {
                        revalidate_all(&hub, &pool).await;
                    },
                    Ok(
                        AppEvent::ConfigChanged(_)
                        | AppEvent::ModulesChanged { .. }
                        | AppEvent::EntitlementChanged(_)
                        | AppEvent::PatreonPost(_)
                        | AppEvent::YoutubeUpload(_)
                        | AppEvent::HostingPaid(_)
                        | AppEvent::ServingChanged(_)
                        | AppEvent::CustomBotsChanged(_)
                        | AppEvent::LoadoutsChanged
                        | AppEvent::TaskScheduled(_)
                        | AppEvent::DeliveryChanged(_)
                        | AppEvent::WebhookBroken { .. },
                    ) => {},
                    Err(RecvError::Closed) => break,
                },
                _ = revalidate.tick() => revalidate_all(&hub, &pool).await,
            }
        }
    })
}

pub async fn apply(
    hub: &WsHub,
    pool: &PgPool,
    event: DeadlockAppEvent,
) -> Result<(), AppApiError> {
    match event {
        DeadlockAppEvent::Patch { client_version } => {
            hub.push_all(&Down::Patch { client_version });
        },
        DeadlockAppEvent::MatchReady { discord_id, match_id } => {
            hub.push_user(discord_id, &Down::MatchReady { match_id });
        },
        DeadlockAppEvent::TokenRevoked { token_id } => {
            hub.close_token(token_id, CLOSE_UNAUTHORIZED);
        },
        DeadlockAppEvent::LinkChanged { discord_id } => {
            let current = match auth::linked_account(pool, discord_id).await {
                Ok(current) => current,
                Err(e) => {
                    for socket in hub.sockets_of(discord_id) {
                        hub.close_socket(socket.id, CLOSE_ACCOUNT_MISMATCH);
                    }
                    return Err(e);
                },
            };
            for socket in hub.sockets_of(discord_id) {
                if Some(socket.account_id) != current {
                    hub.close_socket(socket.id, CLOSE_ACCOUNT_MISMATCH);
                }
            }
        },
    }
    Ok(())
}

pub async fn revalidate_all(hub: &WsHub, pool: &PgPool) {
    for socket in hub.sockets() {
        match verify::check(pool, &socket).await {
            Ok(Some(code)) => hub.close_socket(socket.id, code),
            Ok(None) => {},
            Err(e) => {
                warn!(error = %e, socket = socket.id, "deadlock: websocket revalidation failed");
            },
        }
    }
}
