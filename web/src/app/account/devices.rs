#![expect(
    clippy::empty_enums,
    reason = "path_param! expands to a variantless marker enum"
)]

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::see_other;
use topcoat::router::{page, path_param};
use topcoat::view::{View, ViewExt, attributes, component, view};
use tracing::warn;

use super::page::{PAGE, SIGN_IN_PATH};
use crate::auth::{AuthError, db_pool, remember_login_next, require_user};
use crate::components::alert::{AlertVariant, alert, alert_title};
use crate::components::button::{ButtonSize, ButtonVariant, button_variants};
use crate::deadlock_app::tokens::{AppDevice, list_live, revoke_owned};
use crate::flash::{FlashKind, set as set_flash};
use crate::state::SessionIdentity;

path_param!(token_id: i64 else not_found);

pub(super) async fn load(
    cx: &Cx,
    identity: &SessionIdentity,
) -> Option<Vec<AppDevice>> {
    let pool = match db_pool(cx) {
        Ok(pool) => pool,
        Err(error) => {
            warn!(error = %error, "failed to load the signed-in Deadlock apps");
            return None;
        },
    };
    match list_live(pool, identity.user_id).await {
        Ok(devices) => Some(devices),
        Err(error) => {
            warn!(error = %error, "failed to load the signed-in Deadlock apps");
            None
        },
    }
}

#[page(POST "./{token_id}/revoke")]
pub(super) async fn revoke(cx: &Cx) -> Result<impl View> {
    let identity = match require_user(cx).await {
        Ok(identity) => identity,
        Err(AuthError::Unauthenticated) => {
            remember_login_next(cx, PAGE)?;
            return Err(see_other(SIGN_IN_PATH).into());
        },
        Err(error) => return Err(error.into()),
    };

    match revoke_owned(db_pool(cx)?, *token_id(cx)?, identity.user_id).await {
        Ok(true) => set_flash(cx, FlashKind::Success, "Signed the app out.")?,
        Ok(false) => {
            set_flash(cx, FlashKind::Error, "That app sign-in wasn't found.")?;
        },
        Err(error) => {
            warn!(error = %error, "failed to revoke a Deadlock app sign-in");
            set_flash(
                cx,
                FlashKind::Error,
                "Couldn't sign the app out. Please try again.",
            )?;
        },
    }

    Err::<(), _>(see_other(PAGE).into())
}

#[component]
pub(super) async fn devices_section(
    cx: &Cx,
    devices: Option<&[AppDevice]>,
) -> Result<impl View> {
    Ok(view! {
        <section
            class="flex flex-col gap-3 mt-10"
            aria-labelledby="account-devices-title"
        >
            <h2 class="label" id="account-devices-title">"Deadlock app"</h2>
            <p class="page-lead">
                "Apps signed in to your account. Revoking signs that app out."
            </p>
            match devices {
                None => {
                    alert(
                        variant: AlertVariant::Destructive,
                        attrs: attributes! { class="error" role="alert" },
                        alert_title(
                            "Your signed-in apps couldn't be loaded. Please try again shortly."
                        )
                    )
                }
                Some([]) => {
                    <p class="m-0 text-[13.5px] text-text-2">
                        "No apps are signed in."
                    </p>
                }
                Some(devices) => {
                    <ul class="list-none m-0 p-0 flex flex-col gap-3">
                        #[key(device.id)]
                        for device in devices {
                            device_row(device: device)
                        }
                    </ul>
                }
            }
        </section>
    }
    .boxed())
}

#[component]
async fn device_row(cx: &Cx, device: &AppDevice) -> Result<impl View> {
    let version = format!("Version {}", device.app_version);
    let signed_in = format!("Signed in {}", device.created_at.strftime("%Y-%m-%d"));
    let last_used = device.last_used_at.map_or_else(
        || "Never used".to_owned(),
        |at| format!("Last used {} UTC", at.strftime("%Y-%m-%d %H:%M")),
    );
    let action = format!("/account/devices/{}/revoke", device.id);

    Ok(view! {
        <li class="card flex flex-wrap items-center justify-between gap-4">
            <div class="flex flex-col gap-1 min-w-0">
                <p class="m-0 text-text wrap-anywhere">(device.device_name.as_str())</p>
                <p class="m-0 text-[13.5px] text-text-2">(version)</p>
                <p class="m-0 text-[13.5px] text-text-2">(signed_in)</p>
                <p class="m-0 text-[13.5px] text-text-2">(last_used)</p>
            </div>
            <form method="post" action=(action) data-pending="">
                <button
                    type="submit"
                    class=(button_variants(ButtonVariant::Secondary, ButtonSize::Md))
                    data-pending-label="Revoking\u{2026}"
                >
                    "Revoke"
                </button>
            </form>
        </li>
    })
}
