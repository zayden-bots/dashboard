use std::str::FromStr;

use topcoat::context::Cx;
use topcoat::router::request::uri;
use url::form_urlencoded;
use zayden_app::applications::{
    AppKind,
    ApplicationProfile,
    ApplicationStore,
    Slot,
};
use zayden_app::guilds::GuildPresence;
use zayden_app::serving::ServingKind;
use zayden_app::state::AppState;

use super::error::{GuildError, server_err};
use crate::auth::app_state;
use crate::nav::path_segment;

const BOT_PARAM: &str = "bot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatView {
    pub slot: Slot,
    pub application_id: i64,
    pub kind: AppKind,
    pub present: bool,
    pub name: String,
    pub avatar_url: Option<String>,
}

#[must_use]
pub fn offered_slots(app: &AppState) -> Vec<Slot> {
    Slot::ALL
        .into_iter()
        .filter(|slot| app.default_application_id(*slot).is_some())
        .collect()
}

#[must_use]
pub fn selected_slot(cx: &Cx) -> Slot {
    let Ok(app) = app_state(cx) else {
        return Slot::Zayden;
    };
    let raw = uri(cx).query().and_then(|query| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == BOT_PARAM)
            .map(|(_, value)| value.into_owned())
    });

    parse_bot(app, raw.as_deref()).unwrap_or(Slot::Zayden)
}

#[must_use]
pub const fn bot_param(slot: Slot) -> Option<&'static str> {
    match slot {
        Slot::Zayden => None,
        Slot::Maria => Some(slot.as_str()),
    }
}

#[must_use]
pub fn overview_href(guild_id: &str, slot: Slot) -> String {
    let base = format!("/guild/{}", path_segment(guild_id));
    bot_param(slot).map_or_else(|| base.clone(), |bot| format!("{base}?bot={bot}"))
}

pub fn parse_bot(app: &AppState, raw: Option<&str>) -> Result<Slot, GuildError> {
    let Some(text) = raw else {
        return Ok(Slot::Zayden);
    };
    let slot = Slot::from_str(text).ok().ok_or(GuildError::InvalidField("bot"))?;
    if app.default_application_id(slot).is_none() {
        return Err(GuildError::InvalidField("bot"));
    }
    Ok(slot)
}

pub async fn seat(
    app: &AppState,
    guild_id: i64,
    slot: Slot,
) -> Result<SeatView, GuildError> {
    let serving = app.serving.fetch(guild_id, slot).await.map_err(server_err)?;
    let (application_id, kind, present) = if let Some(bot) = serving {
        let kind = match bot.kind {
            ServingKind::Default => AppKind::Default,
            ServingKind::Custom => AppKind::Custom,
        };
        (bot.application_id.cast_signed(), kind, bot.present)
    } else {
        let application_id = app
            .default_application_id(slot)
            .ok_or(GuildError::InvalidField("bot"))?
            .cast_signed();
        let present = GuildPresence::is_present(&app.db, guild_id, application_id)
            .await
            .map_err(server_err)?;
        (application_id, AppKind::Default, present)
    };

    let profile = ApplicationStore::profile(&app.db, application_id)
        .await
        .map_err(server_err)?;
    let name = profile
        .as_ref()
        .map_or_else(|| slot.name().to_owned(), |profile| profile.name.clone());
    let avatar_url = profile.as_ref().and_then(|profile| {
        profile.avatar.as_ref().map(|hash| {
            format!(
                "https://cdn.discordapp.com/avatars/{}/{hash}.png?size=64",
                profile.bot_user_id
            )
        })
    });

    Ok(SeatView { slot, application_id, kind, present, name, avatar_url })
}

pub async fn trusted_in_guild(
    app: &AppState,
    guild_id: i64,
) -> Result<Vec<ApplicationProfile>, GuildError> {
    let present = ApplicationStore::present_in_guild(&app.db, guild_id)
        .await
        .map_err(server_err)?;

    let mut trusted = Vec::new();
    let mut customs = Vec::new();
    for row in present {
        match row.kind {
            AppKind::Default => trusted.push(row),
            AppKind::Custom => {
                let serving = app
                    .serving
                    .fetch(guild_id, row.slot)
                    .await
                    .map_err(server_err)?;
                if serving.is_some_and(|bot| {
                    bot.application_id.cast_signed() == row.application_id
                }) {
                    customs.push(row);
                }
            },
        }
    }
    trusted.extend(customs);

    Ok(trusted)
}

pub async fn seats(
    app: &AppState,
    guild_id: i64,
) -> Result<Vec<SeatView>, GuildError> {
    let mut views = Vec::new();
    for slot in offered_slots(app) {
        views.push(seat(app, guild_id, slot).await?);
    }
    Ok(views)
}
