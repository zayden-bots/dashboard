#![expect(
    unreachable_pub,
    reason = "#[component] re-emits each fn inside its `Component::render` impl, where `pub` cannot be reached; the marker struct the macro emits carries the real, public visibility"
)]

use topcoat::Result;
use topcoat::asset::{Asset, asset};
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

pub const LOGO: Asset = asset!("../../assets/logo.png");

#[component]
pub async fn brand_mark(cx: &Cx) -> Result<impl View> {
    Ok(view! {
        <img
            class="flex-none w-[28px] h-[28px] rounded-(--radius-full)"
            src=(LOGO)
            alt=""
            width="28"
            height="28"
        >
    })
}
