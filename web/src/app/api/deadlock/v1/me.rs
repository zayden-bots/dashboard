use accounts::links::AccountLink;
use accounts::provider::Provider;
use deadlock::db::user_settings;
use deadlock_protocol::auth::{AccountSettings, Me, Tier};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::response::Response;
use topcoat::router::route;
use zayden_app::entitlement::Tier as EntitlementTier;

use crate::auth::{app_state, db_pool};
use crate::deadlock_app::AppApiError;
use crate::deadlock_app::auth::authenticate;
use crate::deadlock_app::reply::json;

#[route(GET)]
pub(super) async fn me(cx: &Cx) -> Result<Response> {
    json(cx, load(cx).await)
}

#[route(*)]
pub(super) async fn me_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

const fn wire_tier(tier: EntitlementTier) -> Tier {
    match tier {
        EntitlementTier::Free => Tier::Free,
        EntitlementTier::Pro => Tier::Pro,
        EntitlementTier::Ultra => Tier::Ultra,
    }
}

async fn load(cx: &Cx) -> std::result::Result<Me, AppApiError> {
    let caller = authenticate(cx).await?;
    let id = caller.discord_id;
    let pool = db_pool(cx)?;

    let (dm_summaries, patch_briefing, match_fetch) = tokio::try_join!(
        async {
            user_settings::dm_target(pool, id).await.map(|(enabled, _)| enabled)
        },
        user_settings::patch_briefing(pool, id),
        user_settings::match_fetch(pool, id),
    )?;
    let account_id = AccountLink::select(pool, id, Provider::Steam)
        .await?
        .and_then(|link| link.steam_id())
        .map(|steam| steam.account_id().get());
    let tier = app_state(cx)?.entitlements.user_tier(id.cast_unsigned()).await;

    Ok(Me {
        discord_user_id: id.to_string(),
        account_id,
        tier: wire_tier(tier),
        settings: AccountSettings { match_fetch, dm_summaries, patch_briefing },
    })
}
