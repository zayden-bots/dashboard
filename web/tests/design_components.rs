//! Markup contracts of the design-system components: the names, roles and
//! attributes that labels, switches, dialogs, menus and the shared script rely
//! on.

use std::error::Error;

use http_body_util::BodyExt;
use topcoat::Result as ViewResult;
use topcoat::context::Cx;
use topcoat::router::request::Request;
use topcoat::router::{Body, Router, RouterBuilder, page};
use topcoat::runtime::RuntimeLayer;
use topcoat::view::{View, view};
use web::components::data_table::{data_cell, data_row, data_table};
use web::components::empty_state::empty_state;
use web::components::error_panel::{ErrorAction, error_panel};
use web::components::field_row::{
    describedby,
    field_id,
    field_row,
    select_row,
    text_row,
    toggle_row,
};
use web::components::flash::{flash, flash_message};
use web::components::lamp::{LampState, lamp};
use web::components::module_row::{
    ModuleControl,
    NOT_SYNCED_NOTE,
    module_group,
    module_row,
};
use web::components::pickers::SelectOption;
use web::components::plan_note::{plan_note, plan_tag};
use web::components::progress_bar::progress_bar;
use web::components::save_bar::save_bar;
use web::flash::{Flash, FlashKind, MAX_MESSAGE_CHARS};

mod canonical;
mod support;

use canonical::canonical;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const ERROR_ALERT: &str = "grid w-full grid-cols-[0_1fr] items-start gap-y-1 rounded-lg border bg-background px-4 py-3 text-sm has-[>svg]:grid-cols-[1rem_1fr] has-[>svg]:gap-x-3 [&amp;>svg]:size-4 [&amp;>svg]:translate-y-0.5 border-destructive/50 text-destructive";
const BADGE_OUTLINE: &str = "inline-flex w-fit shrink-0 items-center justify-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium whitespace-nowrap border-border text-foreground";
const TABLE: &str = "w-full caption-bottom border-collapse border-border text-sm [:not(:first-child)>&amp;]:border-t [:not(:last-child)>&amp;]:border-b data-table";
const CAPTION: &str = "mt-4 text-sm text-muted-foreground visually-hidden";
const THEAD: &str = "bg-foreground/5 [&amp;_tr]:border-b";
const TBODY: &str = "[&amp;_tr:last-child]:border-0";
const ROW: &str = "border-b border-border transition-colors hover:bg-foreground/5";
const TH: &str = "p-3 text-left align-middle font-medium whitespace-nowrap text-muted-foreground first:pl-6 last:pr-6";
const TD: &str = "p-3 align-middle whitespace-nowrap first:pl-6 last:pr-6";
const LAMP_STATUS: &str = "inline-flex items-center gap-2 whitespace-nowrap";
const LAMP_ON: &str = "lamp-on inline-block w-[10px] h-[10px] shrink-0 box-border rounded-(--radius-full)";
const LAMP_OFF: &str = "inline-block w-[10px] h-[10px] shrink-0 box-border rounded-(--radius-full) border-2 border-solid border-line bg-transparent";
const LAMP_SYNC: &str = "lamp-sync inline-block w-[10px] h-[10px] shrink-0 box-border rounded-(--radius-full)";
const RACK_GROUP: &str = "mb-8";
const RACK_HEADING: &str = "m-0 mb-2 label";
const RACK_ROW: &str = "grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-x-4 min-h-(--row-h) py-3 px-4 border-t border-solid border-t-rule first:[border-top:0] max-md:grid-cols-[minmax(0,1fr)_auto] max-md:gap-y-2 max-md:min-h-[48px]";
const RACK_NAME: &str = "m-0 text-[15px] font-semibold";
const RACK_NAME_LINK: &str = "inline-flex items-center min-h-(--target-min) max-md:min-h-(--row-h) text-text no-underline decoration-current hover:underline";
const RACK_LINK: &str =
    "inline-flex items-center gap-1 min-h-(--target-min) text-text font-medium";
const RACK_NOTE: &str = "col-span-full m-0 mt-2 text-[13px] text-caution";
const SAVE_BAR_STATUS: &str = "flex-1 min-w-[12rem] m-0 text-text-2";
const SAVE_BAR_DIRTY: &str = "text-text font-semibold";
const FLASH_TEXT: &str = "text-text";
const FIELD_HELP: &str = "m-0 text-[13px] text-text-2";
const EMPTY_STATE: &str = "flex flex-col items-start gap-2 max-w-(--form-max) py-8 px-6 border border-dashed border-line rounded-panel";
const EMPTY_TITLE: &str = "m-0 text-[17px] font-semibold";
const EMPTY_TEXT: &str = "m-0 mb-2 text-text-2";
const ERROR_PANEL: &str = "flex flex-col gap-3 max-w-(--form-max) p-6 border border-solid border-danger rounded-panel bg-surface-1";
const ERROR_TEXT: &str = "m-0 text-text-2";
const PLAN_NOTE: &str =
    "flex flex-wrap items-center gap-2 m-0 text-[13px] text-text-2";
const PLAN_LINK: &str =
    "inline-flex items-center min-h-(--target-min) max-md:min-h-(--row-h)";

async fn render(builder: RouterBuilder, path: &str) -> TestResult<String> {
    let router = builder.layer(RuntimeLayer::new()).build();
    let response = router.handle(Request::get(path).body(Body::empty())?).await;
    let bytes = response.into_body().collect().await?.to_bytes();
    let html = String::from_utf8(bytes.to_vec())?;
    Ok(strip_comments(&html))
}

fn strip_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some((before, after)) = rest.split_once("<!--") {
        out.push_str(before);
        rest = after.split_once("-->").map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out
}

fn count(html: &str, needle: &str) -> usize {
    html.matches(needle).count()
}

#[page("/lamp")]
async fn lamp_page(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! {
        lamp(state: LampState::On)
        lamp(state: LampState::Off, text: Some("Disabled"))
        lamp(state: LampState::NotSynced, pending: Some("Saving"))
    })
}

#[tokio::test]
async fn the_lamp_pairs_a_hidden_light_with_its_state_in_words() {
    let html = render(Router::builder().page(lamp_page), "/lamp").await.unwrap();

    assert_eq!(
        html,
        format!(
            concat!(
                r#"<span class="{status}"><span class="{on}" aria-hidden="true"></span><span class="lamp-text">On</span></span>"#,
                r#"<span class="{status}"><span class="{off}" aria-hidden="true"></span><span class="lamp-text">Disabled</span></span>"#,
                r#"<span class="{status}"><span class="{sync}" aria-hidden="true"></span><span class="lamp-text" data-pending-text="Saving">Not synced</span></span>"#,
            ),
            status = LAMP_STATUS,
            on = LAMP_ON,
            off = LAMP_OFF,
            sync = LAMP_SYNC,
        )
    );
}

#[page("/module/switch")]
async fn module_switch(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! {
        module_group(
            title: "Voice & games",
            module_row(
                guild_id: "7",
                module_id: "music",
                label: "Music",
                description: "Play audio in voice channels.",
                control: ModuleControl::Switch { on: true },
                href: Some("/guild/7/settings/music")
            )
        )
    })
}

#[tokio::test]
async fn a_module_switch_is_a_named_role_switch_posting_the_opposite_state() {
    let html = render(Router::builder().page(module_switch), "/module/switch")
        .await
        .unwrap();

    assert!(html.starts_with(&format!(
        r#"<section class="{RACK_GROUP}" aria-labelledby="rack-voice---games"><h2 class="{RACK_HEADING}" id="rack-voice---games">Voice &amp; games</h2><ul class="rack"><li class="{RACK_ROW}">"#
    )), "{html}");
    assert!(canonical(&html).contains(&canonical(&format!(
        r#"<h3 class="{RACK_NAME}"><a href="/guild/7/settings/music" class="{RACK_NAME_LINK}" data-topcoat-link="intent">Music</a></h3>"#
    ))), "{html}");
    assert!(html.contains(
        r#"<form class="contents" method="post" action="/guild/7" data-pending=""><input type="hidden" name="guild" value="7"><input type="hidden" name="module_id" value="music">"#
    ), "{html}");
    assert!(html.contains(
        r#"<button type="submit" class="switch" role="switch" aria-checked="true" aria-label="Music module" name="enabled" value="false"><span class="switch-thumb" aria-hidden="true"></span></button>"#
    ), "{html}");
    assert!(html.contains(r#"data-pending-text="Saving…">On</span>"#), "{html}");
}

#[page("/module/status")]
async fn module_status(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! {
        module_row(
            guild_id: "7",
            module_id: "patreon",
            label: "Patreon",
            description: "Announce new posts.",
            control: ModuleControl::Status { on: false },
            href: Some("/guild/7/settings/patreon"),
            error: Some("Couldn't save.")
        )
        module_row(
            guild_id: "7",
            module_id: "lfg",
            label: "LFG",
            description: "Looking for group.",
            control: ModuleControl::NotSynced
        )
    })
}

#[tokio::test]
async fn status_and_unsynced_rows_have_no_switch() {
    let html = render(Router::builder().page(module_status), "/module/status")
        .await
        .unwrap();

    assert_eq!(count(&html, "role=\"switch\""), 0, "{html}");
    assert_eq!(count(&html, "<form"), 0, "{html}");
    assert!(
        canonical(&html).contains(&canonical(&format!(
            r#"<a href="/guild/7/settings/patreon" class="{RACK_LINK}" data-topcoat-link="intent">Manage"#
        ))),
        "{html}"
    );
    assert!(
        canonical(&html).contains(&canonical(&format!(
            r#"<div class="{ERROR_ALERT} col-span-full m-0 mt-2" role="alert"><p class="col-start-2 font-medium tracking-tight">Couldn't save.</p></div>"#
        ))),
        "{html}"
    );
    assert!(
        html.contains(&format!(r#"<h3 class="{RACK_NAME}">LFG</h3>"#)),
        "{html}"
    );
    assert!(
        html.contains(&format!(
            r#"<span class="{LAMP_SYNC}" aria-hidden="true"></span>"#
        )),
        "{html}"
    );
    assert!(
        html.contains(&format!(r#"<p class="{RACK_NOTE}">{NOT_SYNCED_NOTE}</p>"#)),
        "{html}"
    );
}

#[page("/save-bar")]
async fn save_bar_page(cx: &Cx) -> ViewResult<impl View> {
    let notice = Flash::new(FlashKind::Success, "Saved.");
    Ok(view! { save_bar(notice: Some(&notice)) })
}

#[tokio::test]
async fn the_save_bar_announces_its_state_and_hides_discard_until_dirty() {
    let html =
        render(Router::builder().page(save_bar_page), "/save-bar").await.unwrap();

    assert!(html.starts_with(&format!(
        r#"<div class="save-bar" data-save-bar=""><div class="{SAVE_BAR_STATUS}" role="status"><span class="{SAVE_BAR_DIRTY}" data-dirty-status="" hidden="">Unsaved changes</span><p class="flash flash-success" data-flash="">"#
    )), "{html}");
    assert!(html.contains(&format!(
        r#"<button type="button" class="{}" data-discard="" hidden="">Discard</button><button type="submit" class="{}" data-pending-label="Saving…">Save changes</button></div>"#,
        support::btn("ghost"),
        support::btn("primary")
    )), "{html}");
}

#[page("/flash")]
async fn flash_page(cx: &Cx) -> ViewResult<impl View> {
    let notice = Flash::new(FlashKind::Error, "Couldn't save.");
    Ok(view! {
        flash()
        flash_message(notice: &notice)
    })
}

#[tokio::test]
async fn the_flash_region_is_a_live_status_even_when_empty() {
    let html = render(Router::builder().page(flash_page), "/flash").await.unwrap();

    assert!(html.starts_with(r#"<div class="flash-region" role="status"></div><p class="flash flash-error" data-flash=""><svg"#), "{html}");
    assert!(
        html.ends_with(&format!(
            r#"<span class="{FLASH_TEXT}">Couldn't save.</span></p>"#
        )),
        "{html}"
    );
}

#[test]
fn a_flash_survives_its_cookie_encoding_and_rejects_tampering() {
    let notice = Flash::new(FlashKind::Success, "Role removed.");
    assert_eq!(Flash::decode(&notice.encode()), Some(notice));

    let long = "x".repeat(MAX_MESSAGE_CHARS + 10);
    assert_eq!(
        Flash::new(FlashKind::Error, &long).message.chars().count(),
        MAX_MESSAGE_CHARS
    );
    assert_eq!(Flash::decode("not base64 !"), None);
    assert_eq!(Flash::decode(&Flash::new(FlashKind::Error, "").encode()), None);
}

#[page("/field-row")]
async fn field_row_page(cx: &Cx) -> ViewResult<impl View> {
    let described = describedby("prefix", true, true);
    Ok(view! {
        field_row(
            id: "prefix",
            label: "Prefix",
            help: Some("One or two characters."),
            error: Some("Too long."),
            <input
                class="input"
                id="prefix"
                name="prefix"
                aria-describedby=(described.as_deref())
                aria-invalid="true"
            >
        )
    })
}

#[tokio::test]
async fn a_field_row_labels_its_control_and_names_its_help_and_error() {
    let html =
        render(Router::builder().page(field_row_page), "/field-row").await.unwrap();

    assert_eq!(
        html,
        format!(
            concat!(
                r#"<div class="field-row" data-invalid=""><label class="field-label" for="prefix">Prefix</label>"#,
                r#"<input class="input" id="prefix" name="prefix" aria-describedby="prefix-help prefix-error" aria-invalid="true">"#,
                r#"<p class="{help}" id="prefix-help">One or two characters.</p>"#,
                r#"<p class="field-error" id="prefix-error">Too long.</p></div>"#,
            ),
            help = FIELD_HELP
        )
    );
}

#[test]
fn describedby_names_only_the_lines_that_exist() {
    assert_eq!(describedby("a", false, false), None);
    assert_eq!(describedby("a", true, false).as_deref(), Some("a-help"));
    assert_eq!(describedby("a", false, true).as_deref(), Some("a-error"));
}

#[page("/settings-ids")]
async fn settings_ids(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! {
        text_row(form: "faq-3", name: "title", label: "Title", value: "")
        toggle_row(form: "ai", name: "enabled", label: "Enabled", value: true)
        select_row(
            form: "rr",
            name: "channel_id",
            label: "Channel",
            selected: "",
            options: Ok(
                vec![SelectOption { value: "1".to_owned(), label: "# a".to_owned() }],
            )
        )
    })
}

#[tokio::test]
async fn field_rows_take_their_id_from_the_form_for_repeated_forms() {
    let html =
        render(Router::builder().page(settings_ids), "/settings-ids").await.unwrap();

    for id in ["faq-3-title", "ai-enabled", "rr-channel-id"] {
        assert!(
            html.contains(&format!(r#"<label class="field-label" for="{id}">"#)),
            "{id}: {html}"
        );
        assert_eq!(count(&html, &format!(r#"id="{id}""#)), 1, "{id}: {html}");
    }
    assert_eq!(field_id("faq-new", "channel_id"), "faq-new-channel-id");
}

#[page("/table")]
async fn table_page(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! {
        data_table(
            caption: "Reaction roles",
            columns: &["Role", "Members"],
            data_row(
                data_cell(label: "Role", header: true, "@Mod")
                data_cell(label: "Members", numeric: true, "12")
            )
        )
    })
}

#[tokio::test]
async fn the_data_table_keeps_table_roles_and_cell_labels_for_reflow() {
    let html = render(Router::builder().page(table_page), "/table").await.unwrap();

    assert_eq!(
        canonical(&html),
        canonical(&format!(
            r#"<div class="w-full overflow-x-auto" data-slot="table"><table class="{TABLE}" role="table"><caption class="{CAPTION}">Reaction roles</caption><thead class="{THEAD}" role="rowgroup"><tr class="{ROW}" role="row"><th class="{TH}" scope="col" role="columnheader">Role</th><th class="{TH}" scope="col" role="columnheader">Members</th></tr></thead><tbody class="{TBODY}" role="rowgroup"><tr class="{ROW}" role="row"><th class="{TH}" scope="row" role="rowheader" data-label="Role">@Mod</th><td class="{TD} num" role="cell" data-label="Members">12</td></tr></tbody></table></div>"#
        ))
    );
}

#[page("/empty-and-error")]
async fn empty_and_error(cx: &Cx) -> ViewResult<impl View> {
    let actions = [
        ErrorAction::new("Add Zayden to Guild 7", "/invite?guild=7").external(),
        ErrorAction::new("Back to servers", "/guilds"),
    ];
    Ok(view! {
        empty_state(
            title: "No servers yet",
            text: "Add Zayden to a server you manage.",
            action: Some("Add Zayden"),
            href: Some("/invite"),
            external: true
        )
        error_panel(
            title: "Zayden isn't in this server yet",
            message: "Add it, then come back.",
            actions: &actions,
            alert: true
        )
    })
}

#[tokio::test]
async fn empty_and_error_states_lead_with_one_primary_action() {
    let html = render(Router::builder().page(empty_and_error), "/empty-and-error")
        .await
        .unwrap();

    let canonical_html = canonical(&html);
    assert!(canonical_html.starts_with(&canonical(&format!(
        r#"<div class="{EMPTY_STATE}"><h2 class="{EMPTY_TITLE}">No servers yet</h2><p class="{EMPTY_TEXT}">Add Zayden to a server you manage.</p><a href="/invite" rel="external" class="{}">Add Zayden</a></div>"#,
        support::btn("primary")
    ))), "{html}");
    assert!(html.contains(&format!(
        r#"<section class="{ERROR_PANEL}" role="alert"><h1 class="error-title">Zayden isn't in this server yet</h1><p class="{ERROR_TEXT}">Add it, then come back.</p>"#
    )), "{html}");
    assert!(canonical_html.contains(&canonical(&format!(
        r#"<a href="/invite?guild=7" rel="external" class="{}">Add Zayden to Guild 7</a><a href="/guilds" data-topcoat-link="intent" class="{}">Back to servers</a>"#,
        support::btn("primary"),
        support::btn("secondary")
    ))), "{html}");
    assert_eq!(count(&html, &support::btn("primary")), 2, "{html}");
}

#[page("/plan")]
async fn plan_page(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! {
        plan_note(tier: "Pro", text: "Cooldowns below 60 seconds need Pro.")
        plan_tag(label: "Pro")
    })
}

#[tokio::test]
async fn a_plan_note_names_the_tier_in_text_and_links_the_plans_once() {
    let html = render(Router::builder().page(plan_page), "/plan").await.unwrap();

    assert_eq!(
        canonical(&html),
        canonical(&format!(
            r#"<p class="{PLAN_NOTE}"><span class="{BADGE_OUTLINE}">Pro</span><span>Cooldowns below 60 seconds need Pro.</span><a href="/upgrade" class="{PLAN_LINK}" data-topcoat-link="intent">See plans</a></p><span class="{BADGE_OUTLINE}">Pro</span>"#
        ))
    );
}

#[page("/progress")]
async fn progress_page(cx: &Cx) -> ViewResult<impl View> {
    Ok(view! { progress_bar() })
}

#[tokio::test]
async fn the_progress_bar_is_hidden_from_assistive_tech() {
    let html =
        render(Router::builder().page(progress_page), "/progress").await.unwrap();

    assert_eq!(
        html,
        r#"<div class="nav-progress" data-nav-progress="" aria-hidden="true"></div>"#
    );
}
