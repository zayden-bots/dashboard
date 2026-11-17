#![expect(
    clippy::empty_enums,
    reason = "module_param! expands to a variantless marker enum"
)]

pub mod engagement;
pub mod missing;
pub mod overview;
pub mod settings;

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::{Slot, layout, module_param};
use topcoat::view::{View, view};

use crate::guild::dto::GuildInfo;
use crate::guild::list_manageable_guilds;
use crate::shell::frame;

module_param!(pub guild_id);

async fn switcher_guilds(cx: &Cx) -> Result<Vec<GuildInfo>> {
    match list_manageable_guilds(cx).await {
        Ok(guilds) => Ok(guilds),
        Err(e) => {
            e.redirect_unauthenticated()?;
            Ok(Vec::new())
        },
    }
}

#[layout]
async fn guild_shell(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    let guild_id: &str = guild_id(cx);
    let guilds = switcher_guilds(cx).await?;

    Ok(view! { frame(guild_id: Some(guild_id), guilds: &guilds, (slot)) })
}
