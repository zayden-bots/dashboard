#![expect(
    unreachable_pub,
    reason = "#[component] re-emits each fn inside its `Component::render` impl, where `pub` cannot be reached; the marker struct the macro emits carries the real, public visibility"
)]

use topcoat::Result;
use topcoat::view::{View, ViewExt, attributes, component, view};

use super::alert::{AlertVariant, alert, alert_description, alert_title};
use super::field_row::select_row;
use crate::guild::delivery::{DELIVERY_FIELD, DeliveryStatus, DeliveryView};

#[component]
pub async fn delivery_fields(form: &str, view: &DeliveryView) -> Result<impl View> {
    Ok(view! {
        select_row(
            form: form,
            name: DELIVERY_FIELD,
            label: "Posted by",
            selected: &view.selected,
            options: Ok(view.options.clone()),
            empty: &view.seat_label,
            help: Some(
                "The seat bot is whichever bot holds the Zayden seat in this server.",
            )
        )
        match &view.status {
            DeliveryStatus::Unset => (view! {}.boxed()),
            DeliveryStatus::Paused { bot } => (view! {
                alert(
                    variant: AlertVariant::Destructive,
                    alert_title("Deliveries paused")
                    alert_description(
                        (format!("{bot} isn't in this server, so nothing is posted here."))
                    )
                )
            }.boxed()),
            DeliveryStatus::WebhookPending { bot } => (view! {
                alert(
                    variant: AlertVariant::Neutral,
                    attrs: attributes! { class="warning" },
                    alert_title("Webhook not set up yet")
                    alert_description(
                        (format!("{bot} sets up a webhook in this channel when it next syncs. If this stays, give {bot} Manage Webhooks here."))
                    )
                )
            }.boxed()),
            DeliveryStatus::Ready { bot } => (view! {
                <p class="m-0 text-[13px] text-text-2">(format!("Posted by {bot}."))</p>
            }.boxed()),
        }
    }
    .boxed())
}
