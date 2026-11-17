use topcoat::cookie::CookieLayer;
use topcoat::font::RouterBuilderFontExt;
use topcoat::metadata::Metadata;
use topcoat::router::RouterBuilder;
use topcoat::runtime::{PrefetchMode, RuntimeLayer};

use crate::app;
use crate::document::NOT_FOUND_TITLE;
use crate::font::GEIST;

#[must_use]
pub fn router() -> RouterBuilder {
    app::routes()
        .font(GEIST)
        .with(PrefetchMode::Never)
        .with(Metadata::new().title(NOT_FOUND_TITLE))
        .layer(RuntimeLayer::new())
        .layer(CookieLayer::new())
}
