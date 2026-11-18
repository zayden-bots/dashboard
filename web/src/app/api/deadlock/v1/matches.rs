use accounts::links::AccountLink;
use accounts::provider::Provider;
use deadlock::db::report_waits::{self, NewWait};
use deadlock::db::{proxy_cache, user_settings};
use deadlock::report_fetch::{
    WAIT_FOR,
    cache_key,
    first_attempt,
    metadata_canonical,
    metadata_players,
    report_time_ok,
};
use deadlock_protocol::rest::{MatchReport, MatchReportAck};
use jiff::Timestamp;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::request::Bytes;
use topcoat::router::response::Response;
use topcoat::router::route;

use crate::auth::{db_pool, web_state};
use crate::deadlock_app::AppApiError;
use crate::deadlock_app::auth::authenticate;
use crate::deadlock_app::reply::{json, parse};

#[route(POST)]
pub(super) async fn report(cx: &Cx, body: Bytes) -> Result<Response> {
    json(cx, accept(cx, &body).await)
}

#[route(*)]
pub(super) async fn report_other(cx: &Cx) -> Result<Response> {
    AppApiError::MethodNotAllowed.respond(cx)
}

async fn accept(
    cx: &Cx,
    body: &[u8],
) -> std::result::Result<MatchReportAck, AppApiError> {
    let caller = authenticate(cx).await?;
    web_state(cx)?.app_limits.match_report.check(&caller.token_id)?;

    let request: MatchReport = parse(body)?;
    if !i64::try_from(request.match_id).is_ok_and(|id| id > 0) {
        return Err(AppApiError::BadRequest(
            "match_id must be between 1 and 9223372036854775807".to_owned(),
        ));
    }

    let now = Timestamp::now();
    let ended_at = Timestamp::from_millisecond(request.ended_at_ms)
        .ok()
        .filter(|ended_at| report_time_ok(*ended_at, now))
        .ok_or_else(|| {
            AppApiError::BadRequest(
                "ended_at_ms must be within the last 7 days".to_owned(),
            )
        })?;

    let pool = db_pool(cx)?;
    let account_id = AccountLink::select(pool, caller.discord_id, Provider::Steam)
        .await?
        .and_then(|link| link.steam_id())
        .map(|steam| steam.account_id().get())
        .ok_or(AppApiError::NotLinked)?;
    if account_id != request.account_id {
        return Err(AppApiError::AccountMismatch);
    }

    if !user_settings::match_fetch(pool, caller.discord_id).await? {
        return Ok(MatchReportAck::OptedOut);
    }

    let cached =
        proxy_cache::get(pool, &cache_key(&metadata_canonical(request.match_id)))
            .await?
            .is_some_and(|row| row.fresh && metadata_players(&row.body).is_some());

    let inserted = report_waits::insert(pool, &NewWait {
        account_id,
        match_id: request.match_id,
        discord_id: caller.discord_id,
        ended_at,
        reported_at: now,
        expires_at: now.checked_add(WAIT_FOR).unwrap_or(Timestamp::MAX),
        next_attempt_at: if cached { now } else { first_attempt(ended_at, now) },
    })
    .await?;

    Ok(if inserted && !cached {
        MatchReportAck::Accepted
    } else {
        MatchReportAck::Known
    })
}
