use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{Child, View, component, view};

use super::layout::public_layout;

const CONTACT_EMAIL: &str = "kilooscarsix@gmail.com";

#[component]
pub(crate) async fn legal_document(
    cx: &Cx,
    title: &str,
    updated: &str,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        public_layout(
            <div class="legal">
                <h1>(title)</h1>
                <p class="legal-updated">
                    "Last updated: "
                    (updated)
                </p>
                (child)
            </div>
        )
    })
}

#[component]
pub(crate) async fn contact_email(cx: &Cx) -> Result<impl View> {
    Ok(view! { <strong>(CONTACT_EMAIL)</strong> })
}
