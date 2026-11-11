use topcoat::context::Cx;
use zayden_app::applications::{ApplicationStore, Slot};
use zayden_app::delivery::{DeliveryKind, DeliveryStore};

use super::error::{GuildError, server_err};
use super::seat;
use crate::auth::app_state;
use crate::components::pickers::SelectOption;

pub const DELIVERY_FIELD: &str = "delivery_application_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryChoice {
    Keep,
    Seat,
    Bot(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Unset,
    Paused { bot: String },
    WebhookPending { bot: String },
    Ready { bot: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryView {
    pub options: Vec<SelectOption>,
    pub selected: String,
    pub seat_label: String,
    pub status: DeliveryStatus,
}

pub fn take_delivery_choice(
    pairs: &mut Vec<(String, String)>,
) -> Result<DeliveryChoice, GuildError> {
    let mut values = Vec::new();
    pairs.retain(|(name, value)| {
        let ours = name == DELIVERY_FIELD;
        if ours {
            values.push(value.clone());
        }
        !ours
    });

    match values.as_slice() {
        [] => Ok(DeliveryChoice::Keep),
        [value] if value.is_empty() => Ok(DeliveryChoice::Seat),
        [value] => match value.parse::<i64>() {
            Ok(id) if id > 0 => Ok(DeliveryChoice::Bot(id)),
            _ => Err(GuildError::InvalidField(DELIVERY_FIELD)),
        },
        _ => Err(GuildError::InvalidField(DELIVERY_FIELD)),
    }
}

pub async fn poster(
    cx: &Cx,
    guild_id: i64,
    kind: DeliveryKind,
    choice: DeliveryChoice,
) -> Result<i64, GuildError> {
    let app = app_state(cx)?;
    let pool = &app.db;

    let bound = DeliveryStore::bound_application(pool, kind, guild_id)
        .await
        .map_err(server_err)?;
    let choice = match choice {
        DeliveryChoice::Bot(id) if bound == Some(id) => DeliveryChoice::Keep,
        DeliveryChoice::Bot(id) => DeliveryChoice::Bot(id),
        DeliveryChoice::Keep => DeliveryChoice::Keep,
        DeliveryChoice::Seat => DeliveryChoice::Seat,
    };

    let effective = match choice {
        DeliveryChoice::Bot(id) => {
            let trusted = seat::trusted_in_guild(app, guild_id).await?;
            if trusted.iter().any(|profile| profile.application_id == id) {
                return Ok(id);
            }
            return Err(GuildError::InvalidField(DELIVERY_FIELD));
        },
        DeliveryChoice::Seat => {
            DeliveryStore::effective_application(pool, guild_id, None).await
        },
        DeliveryChoice::Keep => {
            DeliveryStore::effective_application(pool, guild_id, bound).await
        },
    }
    .map_err(server_err)?;

    match effective {
        Some(id) => Ok(id),
        None => Ok(seat::seat(app, guild_id, Slot::Zayden).await?.application_id),
    }
}

pub async fn apply_delivery(
    cx: &Cx,
    guild_id: i64,
    kind: DeliveryKind,
    choice: DeliveryChoice,
) -> Result<(), GuildError> {
    let pool = &app_state(cx)?.db;
    let application_id = match choice {
        DeliveryChoice::Keep => return Ok(()),
        DeliveryChoice::Seat => None,
        DeliveryChoice::Bot(id) => {
            let bound = DeliveryStore::bound_application(pool, kind, guild_id)
                .await
                .map_err(server_err)?;
            if bound == Some(id) {
                return Ok(());
            }
            Some(id)
        },
    };

    DeliveryStore::bind_application(pool, kind, guild_id, application_id)
        .await
        .map(|_changed| ())
        .map_err(server_err)
}

pub async fn delivery_view(
    cx: &Cx,
    guild_id: i64,
    kind: DeliveryKind,
    channel_id: Option<i64>,
) -> Result<DeliveryView, GuildError> {
    let app = app_state(cx)?;
    let pool = &app.db;

    let trusted = seat::trusted_in_guild(app, guild_id).await?;
    let mut options = trusted
        .iter()
        .map(|profile| SelectOption {
            value: profile.application_id.to_string(),
            label: profile.name.clone(),
        })
        .collect::<Vec<_>>();
    let seat = seat::seat(app, guild_id, Slot::Zayden).await?;
    let bound = DeliveryStore::bound_application(pool, kind, guild_id)
        .await
        .map_err(server_err)?;
    if let Some(id) = bound
        && !trusted.iter().any(|profile| profile.application_id == id)
    {
        let name = ApplicationStore::profile(pool, id)
            .await
            .map_err(server_err)?
            .map_or_else(|| "Unknown bot".to_owned(), |profile| profile.name);
        options.push(SelectOption {
            value: id.to_string(),
            label: format!("{name} (not in server)"),
        });
    }

    let status = match channel_id {
        None => DeliveryStatus::Unset,
        Some(channel) => {
            let effective =
                DeliveryStore::effective_application(pool, guild_id, bound)
                    .await
                    .map_err(server_err)?;
            match effective {
                None => DeliveryStatus::Paused {
                    bot: bot_name(pool, bound, &seat.name).await?,
                },
                Some(application_id) => {
                    let hook =
                        DeliveryStore::channel_hook(pool, channel, application_id)
                            .await
                            .map_err(server_err)?;
                    let bot =
                        bot_name(pool, Some(application_id), &seat.name).await?;
                    if hook.is_some_and(|hook| !hook.broken) {
                        DeliveryStatus::Ready { bot }
                    } else {
                        DeliveryStatus::WebhookPending { bot }
                    }
                },
            }
        },
    };

    Ok(DeliveryView {
        options,
        selected: bound.map_or_else(String::new, |id| id.to_string()),
        seat_label: format!("Seat bot ({})", seat.name),
        status,
    })
}

async fn bot_name(
    pool: &sqlx::PgPool,
    application_id: Option<i64>,
    fallback: &str,
) -> Result<String, GuildError> {
    let Some(application_id) = application_id else {
        return Ok(fallback.to_owned());
    };
    let profile =
        ApplicationStore::profile(pool, application_id).await.map_err(server_err)?;
    Ok(profile.map_or_else(|| fallback.to_owned(), |profile| profile.name))
}
