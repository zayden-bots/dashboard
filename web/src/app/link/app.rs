use accounts::links::AccountLink;
use accounts::provider::Provider;
use sqlx::PgPool;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::metadata::Metadata;
use topcoat::router::content::Form;
use topcoat::router::error::see_other;
use topcoat::router::{page, query_params};
use topcoat::view::{View, ViewExt, attributes, component, view};
use tracing::warn;

use crate::app::public::layout::public_layout;
use crate::auth::{
    AuthError,
    db_pool,
    remember_login_next,
    require_user,
    web_state,
};
use crate::components::alert::{AlertVariant, alert, alert_title};
use crate::components::button::{ButtonSize, ButtonVariant, button_variants};
use crate::components::flash::flash;
use crate::deadlock_app::device::{
    PendingRequest,
    approve,
    deny,
    display_user_code,
    find_pending,
    normalize_user_code,
};
use crate::deadlock_app::{AppApiError, LINK_APP_PATH};
use crate::flash::{Flash, FlashKind, set as set_flash, take as take_flash};

pub const LINK_APP_TITLE: &str = "Sign in the Deadlock app - Zayden";
const SIGN_IN_PATH: &str = "/auth/discord";
const STEAM_LINK_PATH: &str = "/account/link/steam";
const CODE_FIELD: &str = "code";
const DECISION_FIELD: &str = "decision";
const INVALID_FLASH: &str = "That code is invalid or has expired.";

#[query_params(error = bad_request)]
pub struct LinkQuery {
    pub code: Option<String>,
}

enum LinkState {
    NeedsSteam,
    Enter,
    Invalid(&'static str),
    Limited(u32),
    Confirm(PendingRequest),
    Failed,
}

#[page(with(Metadata::new().title(LINK_APP_TITLE)))]
pub(crate) async fn link_app(cx: &Cx) -> Result<impl View> {
    let notice = take_flash(cx);
    let query = query_params::<LinkQuery>(cx)?;
    let code = query.code.as_deref().and_then(normalize_user_code);

    let user_id = match require_user(cx).await {
        Ok(identity) => identity.user_id,
        Err(AuthError::Unauthenticated) => {
            let next = code.as_ref().map_or_else(
                || LINK_APP_PATH.to_owned(),
                |code| format!("{LINK_APP_PATH}?code={code}"),
            );
            remember_login_next(cx, &next)?;
            return Err(see_other(SIGN_IN_PATH).into());
        },
        Err(error) => return Err(error.into()),
    };

    let state = match resolve(cx, user_id, query.code.is_some(), code.as_deref())
        .await
    {
        Ok(state) => state,
        Err(error) => {
            warn!(error = %error, "failed to load the Deadlock app sign-in request");
            LinkState::Failed
        },
    };

    Ok(view! { public_layout(link_app_page(notice: notice.as_ref(), state: &state)) })
}

async fn resolve(
    cx: &Cx,
    user_id: i64,
    typed: bool,
    code: Option<&str>,
) -> std::result::Result<LinkState, AppApiError> {
    let pool = db_pool(cx)?;
    if AccountLink::select(pool, user_id, Provider::Steam).await?.is_none() {
        return Ok(LinkState::NeedsSteam);
    }
    if !typed {
        return Ok(LinkState::Enter);
    }
    let Some(code) = code else {
        return Ok(LinkState::Invalid(
            "That code isn't valid. Check the code the app shows and try again.",
        ));
    };
    if let Err(AppApiError::RateLimited { retry_after_secs }) =
        web_state(cx)?.app_limits.code_entry.check(&user_id)
    {
        return Ok(LinkState::Limited(retry_after_secs));
    }

    Ok(find_pending(pool, code).await?.map_or(
        LinkState::Invalid(
            "That code is invalid or has expired. Start sign-in again in the app.",
        ),
        LinkState::Confirm,
    ))
}

#[page(POST)]
pub(super) async fn decide(
    cx: &Cx,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Result<impl View> {
    let user_id = match require_user(cx).await {
        Ok(identity) => identity.user_id,
        Err(AuthError::Unauthenticated) => {
            remember_login_next(cx, LINK_APP_PATH)?;
            return Err(see_other(SIGN_IN_PATH).into());
        },
        Err(error) => return Err(error.into()),
    };
    let field = |name: &str| {
        pairs.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    };

    if let Err(AppApiError::RateLimited { retry_after_secs }) =
        web_state(cx)?.app_limits.code_entry.check(&user_id)
    {
        set_flash(
            cx,
            FlashKind::Error,
            &format!("Too many attempts. Try again in {retry_after_secs} seconds."),
        )?;
    } else if let Some(code) = field(CODE_FIELD).and_then(normalize_user_code) {
        match apply(db_pool(cx)?, user_id, &code, field(DECISION_FIELD)).await {
            Ok((kind, message)) => set_flash(cx, kind, message)?,
            Err(error) => {
                warn!(error = %error, "failed to save a Deadlock app sign-in decision");
                set_flash(
                    cx,
                    FlashKind::Error,
                    "Couldn't save your choice. Please try again.",
                )?;
            },
        }
    } else {
        set_flash(cx, FlashKind::Error, INVALID_FLASH)?;
    }

    Err::<(), _>(see_other(LINK_APP_PATH).into())
}

async fn apply(
    pool: &PgPool,
    user_id: i64,
    code: &str,
    decision: Option<&str>,
) -> std::result::Result<(FlashKind, &'static str), AppApiError> {
    match decision {
        Some("approve") => {
            if AccountLink::select(pool, user_id, Provider::Steam).await?.is_none() {
                return Ok((FlashKind::Error, "Link your Steam account first."));
            }
            if approve(pool, code, user_id).await? {
                Ok((
                    FlashKind::Success,
                    "The Deadlock app is signed in. You can go back to it now.",
                ))
            } else {
                Ok((FlashKind::Error, INVALID_FLASH))
            }
        },
        Some("deny") => {
            if deny(pool, code).await? {
                Ok((
                    FlashKind::Success,
                    "Sign-in denied. The app was not signed in.",
                ))
            } else {
                Ok((FlashKind::Error, INVALID_FLASH))
            }
        },
        _ => Ok((FlashKind::Error, "Choose sign in or deny.")),
    }
}

#[component]
async fn link_app_page(
    cx: &Cx,
    notice: Option<&Flash>,
    state: &LinkState,
) -> Result<impl View> {
    Ok(view! {
        <div class="page w-full mx-auto my-0 py-16 px-6">
            <div class="page-header">
                <div>
                    <h1>"Sign in the Deadlock app"</h1>
                    <p class="page-lead">
                        "Confirm the code the Deadlock app shows to connect it to your account."
                    </p>
                </div>
            </div>
            flash(notice: notice)
            match state {
                LinkState::NeedsSteam => {
                    <p class="m-0 text-text">
                        "Link your Steam account before you sign in the app. Then open this page again; the code stays valid for 10 minutes."
                    </p>
                    <a
                        href=(STEAM_LINK_PATH)
                        rel="external"
                        class=(button_variants(ButtonVariant::Primary, ButtonSize::Md))
                    >
                        "Link Steam"
                    </a>
                }
                LinkState::Enter => {
                    code_form()
                }
                LinkState::Invalid(text) => {
                    alert(
                        variant: AlertVariant::Destructive,
                        attrs: attributes! { class="error" role="alert" },
                        alert_title((*text))
                    )
                    code_form()
                }
                LinkState::Limited(seconds) => {
                    alert(
                        variant: AlertVariant::Destructive,
                        attrs: attributes! { class="error" role="alert" },
                        alert_title(
                            "Too many attempts. Try again in "
                            (seconds)
                            " seconds."
                        )
                    )
                }
                LinkState::Confirm(request) => {
                    confirm_card(request: request)
                }
                LinkState::Failed => {
                    alert(
                        variant: AlertVariant::Destructive,
                        attrs: attributes! { class="error" role="alert" },
                        alert_title("Something went wrong. Please try again shortly.")
                    )
                }
            }
        </div>
    })
}

#[component]
async fn code_form(cx: &Cx) -> Result<impl View> {
    Ok(view! {
        <form
            class="card flex flex-wrap items-end gap-4"
            method="get"
            action=(LINK_APP_PATH)
        >
            <label class="flex flex-col gap-1 text-text">
                <span>"Code from the app"</span>
                <input
                    name=(CODE_FIELD)
                    autocomplete="off"
                    autocapitalize="characters"
                    spellcheck="false"
                    maxlength="16"
                    required=""
                >
            </label>
            <button
                type="submit"
                class=(button_variants(ButtonVariant::Primary, ButtonSize::Md))
            >
                "Continue"
            </button>
        </form>
    })
}

#[component]
async fn confirm_card(cx: &Cx, request: &PendingRequest) -> Result<impl View> {
    let version = format!("App version {}", request.app_version);
    let requested =
        format!("Requested {} UTC", request.requested_at.strftime("%Y-%m-%d %H:%M"));
    let display = display_user_code(&request.user_code);

    Ok(view! {
        <form
            class="card flex flex-col gap-3"
            method="post"
            action=(LINK_APP_PATH)
            data-pending=""
        >
            <h2 class="label">"Sign in the Deadlock app?"</h2>
            <p class="m-0 text-text wrap-anywhere">(request.device_name.as_str())</p>
            <p class="m-0 text-[13.5px] text-text-2">(version)</p>
            <p class="m-0 text-[13.5px] text-text-2">(requested)</p>
            <p class="m-0 text-text"><code>(display)</code></p>
            <p class="m-0 text-[13.5px] text-text-2">
                "Only continue if you started this on your own computer and the app shows this code."
            </p>
            <input type="hidden" name=(CODE_FIELD) value=(request.user_code.as_str())>
            <div class="flex flex-wrap gap-3">
                <button
                    type="submit"
                    name=(DECISION_FIELD)
                    value="approve"
                    class=(button_variants(ButtonVariant::Primary, ButtonSize::Md))
                >
                    "Sign in"
                </button>
                <button
                    type="submit"
                    name=(DECISION_FIELD)
                    value="deny"
                    class=(button_variants(ButtonVariant::Secondary, ButtonSize::Md))
                >
                    "Deny"
                </button>
            </div>
        </form>
    }
    .boxed())
}
