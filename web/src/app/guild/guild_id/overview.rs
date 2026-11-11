use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::see_other;
use topcoat::router::{StatusCode, page, path_param, segment};
use topcoat::runtime::{link_attrs, prefetch_mode};
use topcoat::view::{View, ViewExt, attributes, component, view};
use zayden_app::applications::{AppKind, Slot as Seat};
use zayden_app::modules;

use super::GuildId;
use crate::auth::{AuthError, app_state};
use crate::components::alert::{
    AlertVariant,
    alert,
    alert_description,
    alert_title,
};
use crate::components::badge::{BadgeVariant, badge};
use crate::components::button::{ButtonSize, ButtonVariant, button_variants};
use crate::components::error_panel::{ErrorAction, error_panel};
use crate::components::flash::flash;
use crate::components::module_row::module_group;
use crate::components::tabs::{tabs_list, tabs_trigger};
use crate::flash::{Flash, FlashKind, set_in as set_flash, take as take_flash};
use crate::guild::dto::ModuleView;
use crate::guild::modules::{
    ModuleToggleForm,
    Overlap,
    list_guild_modules,
    passive_overlaps,
    set_module_enabled,
};
use crate::guild::seat::{self, SeatView};
use crate::guild::{GuildError, list_manageable_guilds};
use crate::nav::{self, path_segment};
use crate::shell::module_card;
use crate::util::server_error_text;

segment!(kind = Group);

const LEAD: &str = "Turn modules on or off for this server. A module that's off \
                    has its commands removed from the server.";

struct RackGroup {
    slug: &'static str,
    title: &'static str,
    modules: &'static [&'static str],
}

const RACK: &[RackGroup] = &[
    RackGroup {
        slug: "community",
        title: "Community",
        modules: &["greetings", "family", "gambling", "misc"],
    },
    RackGroup {
        slug: "support",
        title: "Support & safety",
        modules: &["ticket", "honeypot", "moderation"],
    },
    RackGroup {
        slug: "voice",
        title: "Voice & games",
        modules: &[
            "music", "jellyfin", "palworld", "marathon", "deadlock", "hosting",
        ],
    },
    RackGroup {
        slug: "integrations",
        title: "Integrations",
        modules: &["ai", "patreon", "youtube"],
    },
];

const OTHER: RackGroup =
    RackGroup { slug: "other", title: "Other modules", modules: &[] };

pub(crate) fn plain(message: &str) -> &str {
    let prefix = server_error_text("");
    message.strip_prefix(prefix.as_str()).unwrap_or(message)
}

fn section_id(group: &RackGroup) -> String {
    format!("modules-{}", group.slug)
}

fn group_of(module_id: &str) -> &'static RackGroup {
    RACK.iter().find(|group| group.modules.contains(&module_id)).unwrap_or(&OTHER)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToggleFailure {
    module_id: Option<String>,
    message: String,
}

#[page]
pub(super) async fn guild_overview(cx: &Cx) -> Result<impl View> {
    let guild_id: &str = path_param::<GuildId>(cx);
    let notice = take_flash(cx);
    let slot = seat::selected_slot(cx);
    let loaded = load(cx, guild_id, slot).await?;

    Ok(
        view! { overview(guild_id: guild_id, loaded: loaded, notice: notice.as_ref()) },
    )
}

#[page(POST)]
pub(super) async fn toggle_module(
    cx: &Cx,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Result<impl View> {
    let guild_id: &str = path_param::<GuildId>(cx);
    let module_id = pairs
        .iter()
        .find(|(name, _)| name == "module_id")
        .map(|(_, value)| value.clone());

    let (pairs, bots) = split_bot(pairs);
    let requested = requested_slot(cx, &bots);
    let slot = requested.clone().unwrap_or(Seat::Zayden);

    let outcome = match requested {
        Ok(slot) => toggle(cx, guild_id, slot, pairs).await,
        Err(error) => Err(error),
    };
    let error = match outcome {
        Ok((module_id, enabled)) => {
            let label = modules::find(&module_id)
                .map_or(module_id.as_str(), |module| module.label);
            let message =
                format!("{label} turned {}.", if enabled { "on" } else { "off" });
            let section = section_id(group_of(&module_id));
            set_flash(cx, FlashKind::Success, &message, &section)?;
            let location =
                format!("{}#{section}", seat::overview_href(guild_id, slot));
            return Err(see_other(location).into());
        },
        Err(error) => error.redirect_unauthenticated()?,
    };
    let failure = ToggleFailure {
        module_id,
        message: format!("Not changed: {}", plain(&error.to_string())),
    };
    let loaded = load(cx, guild_id, slot).await?;

    Ok(view! {
        (StatusCode::UNPROCESSABLE_ENTITY)
        overview(guild_id: guild_id, loaded: loaded, failure: Some(failure))
    })
}

fn split_bot(pairs: Vec<(String, String)>) -> (Vec<(String, String)>, Vec<String>) {
    let (bots, rest): (Vec<_>, Vec<_>) =
        pairs.into_iter().partition(|(name, _)| name == "bot");

    (rest, bots.into_iter().map(|(_, value)| value).collect())
}

fn requested_slot(
    cx: &Cx,
    bots: &[String],
) -> std::result::Result<Seat, GuildError> {
    let raw = match bots {
        [] => None,
        [one] => Some(one.as_str()),
        _ => return Err(GuildError::InvalidField("bot")),
    };

    seat::parse_bot(app_state(cx)?, raw)
}

async fn toggle(
    cx: &Cx,
    guild_id: &str,
    slot: Seat,
    pairs: Vec<(String, String)>,
) -> std::result::Result<(String, bool), GuildError> {
    let form = ModuleToggleForm::from_pairs(pairs)?;
    if form.guild != guild_id {
        return Err(GuildError::InvalidField("guild"));
    }
    let enabled = form.enabled()?;

    set_module_enabled(cx, guild_id, slot, &form.module_id, enabled).await?;
    Ok((form.module_id, enabled))
}

enum Loaded {
    Modules {
        seats: Vec<SeatView>,
        current: Seat,
        modules: Vec<ModuleView>,
        overlaps: Vec<Overlap>,
    },
    Problem {
        title: String,
        message: String,
        actions: Vec<ErrorAction>,
    },
}

async fn load(cx: &Cx, guild_id: &str, slot: Seat) -> Result<Loaded> {
    let error = match list_guild_modules(cx, guild_id, slot).await {
        Ok(modules) => match loaded_seats(cx, guild_id).await {
            Ok((seats, overlaps)) => {
                return Ok(Loaded::Modules {
                    seats,
                    current: slot,
                    modules,
                    overlaps,
                });
            },
            Err(error) => error,
        },
        Err(error) => error.redirect_unauthenticated()?,
    };
    let back = ErrorAction::new("Back to servers", "/guilds");

    Ok(if error == GuildError::Auth(AuthError::BotNotInGuild) {
        let name = server_name(cx, guild_id).await;
        let bot = seat_name(cx, guild_id, slot).await;
        let mut invite = format!("/invite?guild={}", path_segment(guild_id));
        if let Some(param) = seat::bot_param(slot) {
            invite.push_str("&bot=");
            invite.push_str(param);
        }
        let (message, action) = name.as_deref().map_or_else(
            || {
                (
                    format!(
                        "Add {bot} to this server to turn its modules on or off here."
                    ),
                    format!("Add {bot} to this server"),
                )
            },
            |name| {
                (
                    format!("Add {bot} to {name} to turn its modules on or off here."),
                    format!("Add {bot} to {name}"),
                )
            },
        );
        Loaded::Problem {
            title: format!("{bot} isn't in this server yet"),
            message,
            actions: vec![ErrorAction::new(&action, &invite).external(), back],
        }
    } else if error == GuildError::Auth(AuthError::InvalidGuildId) {
        Loaded::Problem {
            title: "Server not found".to_owned(),
            message: "That address doesn't name a Discord server.".to_owned(),
            actions: vec![back],
        }
    } else if error.is_denied() {
        Loaded::Problem {
            title: "You can't manage this server".to_owned(),
            message: "You need Manage Server in this server to change its modules."
                .to_owned(),
            actions: vec![back],
        }
    } else {
        Loaded::Problem {
            title: "Couldn't load this server's modules".to_owned(),
            message: plain(&error.to_string()).to_owned(),
            actions: vec![
                ErrorAction::new("Try again", &seat::overview_href(guild_id, slot)),
                back,
            ],
        }
    })
}

async fn loaded_seats(
    cx: &Cx,
    guild_id: &str,
) -> std::result::Result<(Vec<SeatView>, Vec<Overlap>), GuildError> {
    let app = app_state(cx)?;
    let id =
        guild_id.parse::<u64>().ok().ok_or(AuthError::InvalidGuildId)?.cast_signed();
    let seats = seat::seats(app, id).await?;
    let overlaps = passive_overlaps(app, id).await.unwrap_or_else(|error| {
        tracing::warn!(%error, "couldn't check for duplicate passive modules");
        Vec::new()
    });

    Ok((seats, overlaps))
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

async fn seat_name(cx: &Cx, guild_id: &str, slot: Seat) -> String {
    let Ok(app) = app_state(cx) else {
        return slot.name().to_owned();
    };
    let Ok(id) = guild_id.parse::<i64>() else {
        return slot.name().to_owned();
    };

    seat::seat(app, id, slot)
        .await
        .map_or_else(|_| slot.name().to_owned(), |seat| seat.name)
}

async fn server_name(cx: &Cx, guild_id: &str) -> Option<String> {
    list_manageable_guilds(cx)
        .await
        .ok()?
        .into_iter()
        .find(|guild| guild.id == guild_id)
        .map(|guild| guild.name)
}

#[component]
async fn overview(
    cx: &Cx,
    guild_id: &str,
    loaded: Loaded,
    #[default] notice: Option<&Flash>,
    #[default] failure: Option<ToggleFailure>,
) -> Result<impl View> {
    Ok(view! {
        <div class="page">
            match loaded {
                Loaded::Problem { title, message, actions } => error_panel(
                    title: &title,
                    message: &message,
                    actions: &actions
                ),
                Loaded::Modules { seats, current, modules, overlaps } => {
                    <div class="page-header">
                        <div>
                            <h1>"Overview"</h1>
                            <p class="page-lead">(LEAD)</p>
                        </div>
                        <a
                            class=(button_variants(
                                ButtonVariant::Secondary,
                                ButtonSize::Md,
                            ))
                            (link_attrs(
                                cx,
                                nav::GENERAL.href(guild_id),
                                prefetch_mode(cx),
                            ))
                        >
                            "Server settings"
                        </a>
                    </div>
                    bot_tabs(guild_id: guild_id, seats: &seats, current: current)
                    absent_alert(guild_id: guild_id, seats: &seats, current: current)
                    overlap_alert(overlaps: &overlaps)
                    flash(notice: notice.filter(|notice| notice.section.is_none()))
                    rack(
                        guild_id: guild_id,
                        modules: &modules,
                        bot: seat::bot_param(current),
                        notice: notice,
                        failure: failure.as_ref()
                    )
                }
            }
        </div>
    }
    .boxed())
}

#[component]
async fn bot_tabs(
    guild_id: &str,
    seats: &[SeatView],
    current: Seat,
) -> Result<impl View> {
    Ok(view! {
        tabs_list(
            #[key(seat.slot.as_str())]
            for seat in seats {
                tabs_trigger(
                    active: seat.slot == current,
                    attrs: attributes! {
                        href=(seat::overview_href(guild_id, seat.slot))
                    },
                    if let Some(url) = seat.avatar_url.as_deref() {
                        <img src=(url) alt="" class="size-5 rounded-full">
                    }
                    (seat.name.as_str())
                    if seat.kind == AppKind::Custom {
                        badge(variant: BadgeVariant::Outline, "Custom")
                    }
                    if !seat.present {
                        badge(variant: BadgeVariant::Secondary, "Not in server")
                    }
                )
            }
        )
    }
    .boxed())
}

#[component]
async fn absent_alert(
    guild_id: &str,
    seats: &[SeatView],
    current: Seat,
) -> Result<impl View> {
    let absent = seats.iter().find(|seat| seat.slot == current && !seat.present);
    let title = absent.map(|seat| format!("{} isn't in this server yet", seat.name));
    let add = absent.filter(|seat| seat.kind == AppKind::Default).map(|seat| {
        let mut href = format!("/invite?guild={}", path_segment(guild_id));
        if let Some(bot) = seat::bot_param(seat.slot) {
            href.push_str("&bot=");
            href.push_str(bot);
        }
        (format!("Add {}", seat.name), href)
    });

    Ok(view! {
        if let Some(title) = title.as_deref() {
            alert(
                alert_title((title))
                alert_description(
                    "Its switches reset to the defaults when it joins."
                    if let Some((text, href)) = add.as_ref() {
                        <a
                            class=(button_variants(
                                ButtonVariant::Secondary,
                                ButtonSize::Md,
                            ))
                            href=(href.as_str())
                            rel="external"
                        >
                            (text.as_str())
                        </a>
                    }
                )
            )
        }
    }
    .boxed())
}

#[component]
async fn overlap_alert(overlaps: &[Overlap]) -> Result<impl View> {
    let lines: Vec<String> = overlaps
        .iter()
        .map(|overlap| {
            format!(
                "{} is on for {}. Members may see it act twice.",
                overlap.module,
                join_names(&overlap.bots)
            )
        })
        .collect();

    Ok(view! {
        if !lines.is_empty() {
            alert(
                alert_title("Running on more than one bot")
                alert_description(
                    #[key(line.as_str())]
                    for line in &lines {
                        <p class="m-0">(line.as_str())</p>
                    }
                )
            )
        }
    }
    .boxed())
}

#[component]
async fn rack(
    guild_id: &str,
    modules: &[ModuleView],
    notice: Option<&Flash>,
    failure: Option<&ToggleFailure>,
    #[default] bot: Option<&str>,
) -> Result<impl View> {
    let failed_module = failure.and_then(|failure| failure.module_id.as_deref());
    let unattached = failure
        .filter(|_| {
            !modules.iter().any(|module| Some(module.id.as_str()) == failed_module)
        })
        .map(|failure| failure.message.as_str());
    let others: Vec<&ModuleView> = modules
        .iter()
        .filter(|module| {
            !RACK.iter().any(|group| group.modules.contains(&module.id.as_str()))
        })
        .collect();
    let mut groups: Vec<(&RackGroup, Vec<&ModuleView>)> = RACK
        .iter()
        .map(|group| {
            let rows = group
                .modules
                .iter()
                .filter_map(|id| modules.iter().find(|module| module.id == *id))
                .collect();
            (group, rows)
        })
        .collect();
    groups.push((&OTHER, others));
    groups.retain(|(_, rows)| !rows.is_empty());

    Ok(view! {
        if let Some(message) = unattached {
            alert(
                variant: AlertVariant::Destructive,
                attrs: attributes! { class="error" role="alert" },
                alert_title((message))
            )
        }
        #[key(group.slug)]
        for (group, rows) in &groups {
            let id = section_id(group);
            let group_notice = notice.filter(|notice| notice.is_for(&id));
            <div id=(id.as_str())>
                if group_notice.is_some() {
                    flash(notice: group_notice)
                }
                module_group(
                    title: group.title,
                    #[key(module.id.as_str())]
                    for module in rows {
                        let error = failure
                            .filter(|_| failed_module == Some(module.id.as_str()))
                            .map(|failure| failure.message.as_str());
                        module_card(
                            module: module,
                            guild_id: guild_id,
                            error: error,
                            bot: bot
                        )
                    }
                )
            </div>
        }
    }
    .boxed())
}
