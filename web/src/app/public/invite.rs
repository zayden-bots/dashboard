use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::see_other;
use topcoat::router::request::uri;
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{StatusCode, route};
use url::{Url, form_urlencoded};
use zayden_app::applications::Slot;

use crate::auth::web_state;
use crate::guild::seat;

const GUILD_PARAM: &str = "guild";
const BOT_PARAM: &str = "bot";

const AUTHORIZE_HOSTS: &[&str] = &[
    "discord.com",
    "www.discord.com",
    "ptb.discord.com",
    "canary.discord.com",
    "discordapp.com",
];

const PRESELECT_KEYS: [&str; 2] = ["guild_id", "disable_guild_select"];

fn requested_guild(cx: &Cx) -> Option<u64> {
    let (_, raw) = form_urlencoded::parse(uri(cx).query()?.as_bytes())
        .find(|(key, _)| key == GUILD_PARAM)?;

    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u64>().ok().filter(|id| *id != 0)
}

fn is_authorize_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    let Some(rest) = path.strip_suffix("/oauth2/authorize") else {
        return false;
    };
    rest.strip_prefix("/api").map_or(rest.is_empty(), |version| {
        version.is_empty()
            || version.strip_prefix("/v").is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            })
    })
}

fn requested_bot(cx: &Cx) -> Option<String> {
    let (_, raw) = form_urlencoded::parse(uri(cx).query()?.as_bytes())
        .find(|(key, _)| key == BOT_PARAM)?;

    Some(raw.into_owned())
}

fn authorize_url(address: &str) -> Option<Url> {
    let url = Url::parse(address).ok()?;
    let discord = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AUTHORIZE_HOSTS.contains(&host))
        && is_authorize_path(url.path());

    discord.then_some(url)
}

fn with_client_id(address: &str, application_id: u64) -> Option<String> {
    let mut url = authorize_url(address)?;
    let id = application_id.to_string();

    let mut replaced = false;
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            if key == "client_id" {
                replaced = true;
                (key.into_owned(), id.clone())
            } else {
                (key.into_owned(), value.into_owned())
            }
        })
        .collect();
    let mut query = url.query_pairs_mut();
    query.clear().extend_pairs(pairs);
    if !replaced {
        query.append_pair("client_id", &id);
    }
    drop(query);

    Some(url.into())
}

fn preselect(address: &str, guild: u64) -> Option<String> {
    let mut url = authorize_url(address)?;

    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !PRESELECT_KEYS.contains(&key.as_ref()))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair("guild_id", &guild.to_string())
        .append_pair("disable_guild_select", "true");

    Some(url.into())
}

#[route(GET)]
pub(crate) async fn invite(cx: &Cx) -> Result<Response> {
    let web = web_state(cx)?;
    let Some(configured) = web.urls.invite.as_deref() else {
        return StatusCode::NOT_FOUND.into_response(cx);
    };
    let address = match requested_bot(cx) {
        None => configured.to_owned(),
        Some(raw) => match seat::parse_bot(&web.app, Some(&raw)) {
            Ok(Slot::Zayden) => configured.to_owned(),
            Ok(slot) => {
                let swapped = web
                    .app
                    .default_application_id(slot)
                    .and_then(|id| with_client_id(configured, id));
                let Some(swapped) = swapped else {
                    return StatusCode::NOT_FOUND.into_response(cx);
                };
                swapped
            },
            Err(_) => return StatusCode::NOT_FOUND.into_response(cx),
        },
    };
    let target = requested_guild(cx)
        .and_then(|guild| preselect(&address, guild))
        .unwrap_or(address);

    see_other(target).into_response(cx)
}
