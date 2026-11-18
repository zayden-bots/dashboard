pub mod account;
pub mod admin;
pub mod api;
pub mod auth;
pub mod guild;
pub mod guilds;
pub mod link;
pub mod providers;
pub mod public;
pub mod upgrade;

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::{RouterBuilder, Slot, layout, module_router, not_found};
use topcoat::view::{View, view};

use crate::document::document_layout;

not_found!();

#[must_use]
pub(crate) fn routes() -> RouterBuilder {
    module_router!()
}

#[layout]
async fn root_layout(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    Ok(view! { document_layout(slot: slot) })
}
