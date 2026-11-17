use topcoat::Result;
use topcoat::asset::{Asset, asset};
use topcoat::context::Cx;
use topcoat::metadata::{Metadata, metadata, tag};
use topcoat::router::error::NotFoundError;
use topcoat::router::{Slot, StatusCode};
use topcoat::view::{Child, View, component, error_boundary, view};

use crate::app::admin::d2::loadouts::editor::{
    LOADOUT_NOT_FOUND_TITLE,
    missing_loadout,
};
use crate::components::brand::LOGO;
use crate::error::MissingPage;
use crate::font::font_head;
use crate::guild::seat;
use crate::pages::not_found::not_found_page;

pub const STYLESHEET: Asset = asset!("{out_dir}/tailwind.css");

pub const PENDING_SUBMIT: Asset = asset!("../assets/pending-submit.js");

pub const NOT_FOUND_TITLE: &str = "Not found - Zayden";

#[component]
pub(crate) async fn document_layout(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        error_boundary(
            fallback: |error| {
                let missing = error.downcast_ref::<MissingPage>().copied();
                if missing.is_none() && error.downcast_ref::<NotFoundError>().is_none() {
                    return Err(error);
                }

                Ok(
                    view! {
                        (StatusCode::NOT_FOUND)
                        missing_document(missing: missing)
                    },
                )
            },
            document(bot: Some(seat::selected_slot(cx).as_str()), (slot))
        )
    })
}

#[component]
async fn missing_document(
    cx: &Cx,
    missing: Option<MissingPage>,
) -> Result<impl View> {
    let title = match missing {
        Some(MissingPage::Loadout) => LOADOUT_NOT_FOUND_TITLE,
        None => NOT_FOUND_TITLE,
    };
    let cx = cx.with(Metadata::new().title(title).no_description());

    Ok(view! {
        match missing {
            Some(MissingPage::Loadout) => document(missing_loadout()),
            None => document(not_found_page()),
        }
    })
}

#[component]
async fn document(
    cx: &Cx,
    #[default] bot: Option<&str>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let page = metadata(cx).await?;

    Ok(view! {
        <!DOCTYPE html>
        <html lang="en" class="dark" data-bot=(bot.unwrap_or("zayden"))>
            <head>
                tag::charset()
                tag::viewport(metadata: page)
                <meta name="color-scheme" content="dark">
                tag::description(metadata: page)
                font_head()
                <link rel="icon" type="image/png" href=(LOGO)>
                <link rel="stylesheet" href=(STYLESHEET)>
                topcoat::runtime::script()
                <script type="module" src=(PENDING_SUBMIT)></script>
                tag::title(metadata: page)
            </head>
            <body>
                <a class="skip-link" href="#main">"Skip to main content"</a>
                (child)
            </body>
        </html>
    })
}
