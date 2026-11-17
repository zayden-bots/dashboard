use topcoat::cookie::CookieLayer;
use topcoat::font::RouterBuilderFontExt;
use topcoat::router::RouterBuilder;
use topcoat::runtime::{PrefetchMode, RuntimeLayer};

use crate::app;
use crate::font::GEIST;

#[must_use]
pub fn router() -> RouterBuilder {
    app::routes()
        .font(GEIST)
        .with(PrefetchMode::Never)
        .layer(RuntimeLayer::new())
        .layer(CookieLayer::new())
}
