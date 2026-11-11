use std::collections::HashMap;
use std::sync::Arc;

use topcoat::context::Cx;
use twilight_http::api_error::ApiError;
use twilight_http::error::ErrorType;
use twilight_http::response::marker::ListBody;
use twilight_http::{Client, Error, Response};
use twilight_model::application::command::Command;
use twilight_model::application::command::permissions::{
    CommandPermission,
    CommandPermissionType,
};
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ApplicationMarker,
    ChannelMarker,
    CommandMarker,
    GuildMarker,
    RoleMarker,
};
use zayden_app::applications::Slot;

use super::error::{GuildError, server_err};
use super::seat;
use crate::auth::{
    AuthError,
    GuildAccess,
    app_state,
    bearer_client,
    discord_client,
    guild_admin_context,
    web_state,
};

pub const MAX_ALLOWED_CHANNELS: usize = 90;

#[must_use]
pub const fn all_channels(guild_id: Id<GuildMarker>) -> Option<Id<ChannelMarker>> {
    Id::new_checked(guild_id.get().saturating_sub(1))
}

#[must_use]
pub const fn everyone(guild_id: Id<GuildMarker>) -> Id<RoleMarker> {
    guild_id.cast()
}

#[must_use]
pub fn everyone_denied(
    guild_id: Id<GuildMarker>,
    permissions: &[CommandPermission],
) -> bool {
    permissions.iter().any(|p| {
        !p.permission
            && matches!(p.id, CommandPermissionType::Role(role) if role == everyone(guild_id))
    })
}

#[must_use]
pub fn with_everyone_denied(
    guild_id: Id<GuildMarker>,
    permissions: &[CommandPermission],
    denied: bool,
) -> Vec<CommandPermission> {
    let everyone = everyone(guild_id);

    let mut out = permissions
        .iter()
        .filter(|p| !matches!(p.id, CommandPermissionType::Role(role) if role == everyone))
        .cloned()
        .collect::<Vec<_>>();

    if denied {
        out.push(CommandPermission {
            id: CommandPermissionType::Role(everyone),
            permission: false,
        });
    }

    out
}

#[must_use]
pub fn channel_allowlist(
    guild_id: Id<GuildMarker>,
    permissions: &[CommandPermission],
) -> Vec<Id<ChannelMarker>> {
    let Some(all) = all_channels(guild_id) else {
        return Vec::new();
    };

    let restricted = permissions.iter().any(|p| {
        !p.permission
            && matches!(p.id, CommandPermissionType::Channel(channel) if channel == all)
    });

    if !restricted {
        return Vec::new();
    }

    permissions
        .iter()
        .filter_map(|p| match p.id {
            CommandPermissionType::Channel(channel)
                if p.permission && channel != all =>
            {
                Some(channel)
            },
            CommandPermissionType::Channel(_)
            | CommandPermissionType::Role(_)
            | CommandPermissionType::User(_) => None,
        })
        .collect()
}

#[must_use]
pub fn with_channel_allowlist(
    guild_id: Id<GuildMarker>,
    permissions: &[CommandPermission],
    allowlist: &[Id<ChannelMarker>],
) -> Vec<CommandPermission> {
    let mut out = permissions
        .iter()
        .filter(|p| !matches!(p.id, CommandPermissionType::Channel(_)))
        .cloned()
        .collect::<Vec<_>>();

    let Some(all) = all_channels(guild_id) else {
        return out;
    };

    if allowlist.is_empty() {
        return out;
    }

    out.push(CommandPermission {
        id: CommandPermissionType::Channel(all),
        permission: false,
    });

    for channel in allowlist {
        if *channel == all {
            continue;
        }

        out.push(CommandPermission {
            id: CommandPermissionType::Channel(*channel),
            permission: true,
        });
    }

    out
}

pub struct GuildContext {
    pub guild_id: Id<GuildMarker>,
    pub access_token: String,
    pub http: Arc<Client>,
    pub app_id: Id<ApplicationMarker>,
    pub access: GuildAccess,
    pub bot_name: String,
    pub cross_app: bool,
}

#[must_use]
pub const fn sign_in_rejected(status: u16, code: Option<u64>) -> bool {
    matches!(status, 401 | 403) || matches!(code, Some(50001 | 50013 | 20012))
}

pub async fn guild_context(
    cx: &Cx,
    guild: &str,
) -> Result<GuildContext, GuildError> {
    let ctx = guild_admin_context(cx, guild).await?;
    let seat = seat::seat(app_state(cx)?, ctx.guild_id, Slot::Zayden).await?;
    let app_id = Id::new_checked(seat.application_id.cast_unsigned())
        .ok_or(GuildError::InvalidApplicationId)?;
    let http = match web_state(cx)?.app_client(seat.application_id).await {
        Some(http) => http,
        None => Arc::clone(discord_client(cx)?),
    };

    let cross_app = seat.application_id.cast_unsigned() != app_state(cx)?.zayden_id;

    Ok(GuildContext {
        guild_id: Id::new_checked(ctx.guild_id.cast_unsigned())
            .ok_or(AuthError::InvalidGuildId)?,
        access_token: ctx.access_token,
        http,
        app_id,
        access: ctx.access,
        bot_name: seat.name,
        cross_app,
    })
}

fn read_client(ctx: &GuildContext) -> Arc<Client> {
    match ctx.access {
        GuildAccess::Member => Arc::new(bearer_client(&ctx.access_token)),
        GuildAccess::Operator => Arc::clone(&ctx.http),
    }
}

async fn command_ids(
    list: Result<Response<ListBody<Command>>, Error>,
) -> Result<HashMap<String, Id<CommandMarker>>, GuildError> {
    let commands = list?.models().await?;

    Ok(commands.into_iter().filter_map(|c| c.id.map(|id| (c.name, id))).collect())
}

pub async fn fetch_command_ids(
    ctx: &GuildContext,
) -> Result<HashMap<String, Id<CommandMarker>>, GuildError> {
    let interaction = ctx.http.interaction(ctx.app_id);
    let (global, guild) = tokio::join!(
        interaction.global_commands(),
        interaction.guild_commands(ctx.guild_id),
    );

    let mut merged = command_ids(global).await?;
    merged.extend(command_ids(guild).await?);
    Ok(merged)
}

pub async fn lookup_command_id(
    ctx: &GuildContext,
    name: &str,
) -> Result<Option<Id<CommandMarker>>, GuildError> {
    Ok(fetch_command_ids(ctx).await?.get(name).copied())
}

pub async fn command_id(
    ctx: &GuildContext,
    name: &str,
) -> Result<Id<CommandMarker>, GuildError> {
    lookup_command_id(ctx, name)
        .await?
        .ok_or_else(|| GuildError::CommandNotRegistered(name.to_owned()))
}

const fn unconfigured(error: &Error) -> bool {
    // Discord answers 404 for a command that carries no permission overwrites
    // at all
    matches!(error.kind(), ErrorType::Response { status, .. } if status.get() == 404)
}

pub async fn fetch(
    ctx: &GuildContext,
    command: Id<CommandMarker>,
) -> Result<Vec<CommandPermission>, GuildError> {
    let resp = match read_client(ctx)
        .interaction(ctx.app_id)
        .command_permissions(ctx.guild_id, command)
        .await
    {
        Ok(resp) => resp,
        Err(e) if unconfigured(&e) => return Ok(Vec::new()),
        Err(e) => return Err(read_rejection(&e, &ctx.bot_name, ctx.cross_app)),
    };

    Ok(resp.model().await?.permissions)
}

pub async fn store(
    ctx: &GuildContext,
    command: Id<CommandMarker>,
    name: &str,
    permissions: &[CommandPermission],
) -> Result<(), GuildError> {
    if !ctx.access.can_write_command_permissions() {
        return Err(GuildError::OperatorCommandPermissions(name.to_owned()));
    }

    bearer_client(&ctx.access_token)
        .interaction(ctx.app_id)
        .update_command_permissions(ctx.guild_id, command, permissions)
        .await
        .map(|_resp| ())
        .map_err(|e| write_rejection(&e, name, &ctx.bot_name, ctx.cross_app))
}

const fn cross_app_rejected(error: &Error, cross_app: bool) -> bool {
    if !cross_app {
        return false;
    }

    if let ErrorType::Response { status, error: api, .. } = error.kind() {
        let code = if let ApiError::General(general) = api {
            Some(general.code)
        } else {
            None
        };
        sign_in_rejected(status.get(), code)
    } else {
        matches!(error.kind(), ErrorType::Unauthorized)
    }
}

#[must_use]
pub fn read_rejection(error: &Error, bot: &str, cross_app: bool) -> GuildError {
    if cross_app_rejected(error, cross_app) {
        GuildError::PermissionsNotAccepted { bot: bot.to_owned() }
    } else {
        server_err(error)
    }
}

#[must_use]
pub fn write_rejection(
    error: &Error,
    name: &str,
    bot: &str,
    cross_app: bool,
) -> GuildError {
    if cross_app_rejected(error, cross_app) {
        GuildError::PermissionsNotAccepted { bot: bot.to_owned() }
    } else {
        GuildError::PermissionUpdateRejected {
            name: name.to_owned(),
            reason: error.to_string(),
        }
    }
}
