#![expect(
    unreachable_pub,
    reason = "#[component] re-emits each fn inside its `Component::render` impl, where `pub` cannot be reached; the marker struct the macro emits carries the real, public visibility"
)]

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::href;
use topcoat::router::request::uri;
use topcoat::runtime::{PrefetchMode, link_attrs};
use topcoat::view::{View, ViewExt, class, component, view};

use super::badge::{BadgeVariant, badge_variants};
use super::icons::{Icon, icon};
use crate::admin::{guild_operator_access, is_admin, is_operator};
use crate::app::admin::d2::loadouts::loadouts_page;
use crate::app::admin::servers::servers;
use crate::app::admin::status::status;
use crate::app::guild::guild_id;
use crate::app::guild::guild_id::overview::guild_overview;
use crate::app::guilds::guilds;
use crate::app::upgrade::upgrade;
use crate::nav::{GENERAL, GROUPS, ModuleNav};
use crate::shell::link::is_active_for;

const OVERVIEW: &str = "Overview";
const ALL_SERVERS: &str = "Servers";
const PLANS: &str = "Plans";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavAccess {
    pub operator: bool,
    pub admin: bool,
    pub on_operator_page: bool,
}

impl NavAccess {
    pub async fn load(cx: &Cx, guild_id: Option<&str>) -> Self {
        let (operator, admin) = tokio::join!(is_operator(cx), is_admin(cx));
        let (operator, admin) = (operator.unwrap_or(false), admin.unwrap_or(false));
        let on_operator_page = match guild_id {
            Some(guild_id) if operator => {
                guild_operator_access(cx, guild_id).await.unwrap_or(false)
            },
            _ => false,
        };

        Self { operator, admin, on_operator_page }
    }
}

fn current(href: &str, location: &str, exact: bool) -> Option<&'static str> {
    let on = if exact { href == location } else { is_active_for(href, location) };
    on.then_some("page")
}

#[component]
pub async fn nav_links(
    cx: &Cx,
    prefix: &str,
    access: NavAccess,
    #[default] guild_id: Option<&str>,
) -> Result<impl View> {
    let location = uri(cx).path();
    let NavAccess { operator, admin, on_operator_page } = access;
    let overview =
        guild_id.map(|guild_id| href!(guild_overview, guild_id::guild_id: guild_id));
    let overview_current = overview
        .as_ref()
        .and_then(|overview| overview.is_current(cx).then_some("page"));
    let overview = overview.map(|overview| overview.resolve(cx));
    let settings = guild_id.map(|guild_id| GENERAL.href(guild_id));
    let all_servers = href!(guilds);
    let all_servers_current = all_servers.is_current(cx).then_some("page");
    let all_servers = all_servers.resolve(cx);
    let plans = href!(upgrade).resolve(cx);
    let bot_servers = href!(servers);
    let bot_servers_current = bot_servers.is_current(cx).then_some("page");
    let bot_servers = bot_servers.resolve(cx);
    let bot_status = href!(status);
    let bot_status_current = bot_status.is_current(cx).then_some("page");
    let bot_status = bot_status.resolve(cx);
    let loadouts = href!(loadouts_page).resolve(cx);

    Ok(view! {
        <nav class="flex flex-col gap-1" aria-label="Dashboard">
            if on_operator_page {
                <div
                    class=(class!(badge_variants(BadgeVariant::Outline), "operator-badge"))
                >
                    icon(name: Icon::Shield)
                    <span>"Operator access"</span>
                </div>
            }
            if let (Some(guild_id), Some(overview), Some(settings)) = (
                guild_id,
                overview.as_deref(),
                settings.as_deref(),
            ) {
                <ul class="flex flex-col gap-1 m-0 p-0 list-none">
                    nav_item(
                        href: overview,
                        label: OVERVIEW,
                        icon_name: Some(Icon::Grid),
                        aria_current: overview_current
                    )
                    nav_item(
                        href: settings,
                        label: GENERAL.label,
                        icon_name: Some(Icon::Settings),
                        aria_current: current(settings, location, false)
                    )
                </ul>
                #[key(group.label)]
                for group in GROUPS {
                    nav_group(
                        prefix: prefix,
                        label: group.label,
                        guild_id: guild_id,
                        entries: group.entries,
                        location: location
                    )
                }
            } else {
                <ul class="flex flex-col gap-1 m-0 p-0 list-none">
                    nav_item(
                        href: &all_servers,
                        label: ALL_SERVERS,
                        icon_name: Some(Icon::Server),
                        aria_current: all_servers_current
                    )
                </ul>
            }
            <ul class="flex flex-col gap-1 m-0 mt-4 p-0 list-none">
                nav_item(
                    href: &plans,
                    label: PLANS,
                    icon_name: Some(Icon::Zap),
                    aria_current: current(&plans, location, false)
                )
            </ul>
            if operator || admin {
                <div class="flex flex-col gap-1 mt-4">
                    <p
                        class="m-0 py-1 px-3 font-mono text-[12px] font-medium tracking-[0.06em] uppercase text-text-3"
                        id=(format!("{prefix}-admin"))
                    >
                        "Admin"
                    </p>
                    <ul
                        class="flex flex-col gap-1 m-0 p-0 list-none"
                        aria-labelledby=(format!("{prefix}-admin"))
                    >
                        if operator {
                            nav_item(
                                href: &bot_servers,
                                label: "All bot servers",
                                icon_name: Some(Icon::Shield),
                                aria_current: bot_servers_current
                            )
                            nav_item(
                                href: &bot_status,
                                label: "Bot status",
                                icon_name: Some(Icon::Shield),
                                aria_current: bot_status_current
                            )
                        }
                        if admin {
                            nav_item(
                                href: &loadouts,
                                label: "Loadout builder",
                                icon_name: Some(Icon::Gamepad),
                                aria_current: current(&loadouts, location, false)
                            )
                        }
                    </ul>
                </div>
            }
        </nav>
    }
    .boxed())
}

#[component]
async fn nav_group(
    cx: &Cx,
    prefix: &str,
    label: &str,
    guild_id: &str,
    entries: &[ModuleNav],
    location: &str,
) -> Result<impl View> {
    let heading = format!(
        "{prefix}-{}",
        label
            .to_ascii_lowercase()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    );

    Ok(view! {
        <div class="flex flex-col gap-1 mt-4">
            <p
                class="m-0 py-1 px-3 font-mono text-[12px] font-medium tracking-[0.06em] uppercase text-text-3"
                id=(heading.as_str())
            >
                (label)
            </p>
            <ul
                class="flex flex-col gap-1 m-0 p-0 list-none"
                aria-labelledby=(heading.as_str())
            >
                #[key(entry.label)]
                for entry in entries {
                    let href = entry.href(guild_id);
                    nav_item(
                        href: &href,
                        label: entry.label,
                        aria_current: current(&href, location, false)
                    )
                }
            </ul>
        </div>
    })
}

#[component]
async fn nav_item(
    cx: &Cx,
    href: &str,
    label: &str,
    #[default] icon_name: Option<Icon>,
    #[default] aria_current: Option<&str>,
) -> Result<impl View> {
    Ok(view! {
        <li>
            <a
                class="nav-link"
                aria-current=(aria_current)
                (link_attrs(cx, href, PrefetchMode::Intent))
            >
                if let Some(name) = icon_name {
                    icon(name: name)
                }
                <span>(label)</span>
            </a>
        </li>
    })
}
