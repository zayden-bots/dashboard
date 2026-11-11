#![expect(
    unreachable_pub,
    reason = "#[component] re-emits each fn inside its `Component::render` impl, where `pub` cannot be reached; the marker struct the macro emits carries the real, public visibility"
)]

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::page;
use topcoat::view::{View, ViewExt, attributes, component, view};

use super::gate::{Gate, closed, gate};
use crate::admin::AdminError;
use crate::admin::status::{ProcessView, process_status};
use crate::auth::WebRole;
use crate::components::alert::{AlertVariant, alert, alert_title};
use crate::components::badge::{BadgeVariant, badge};
use crate::components::table::{
    table,
    table_body,
    table_cell,
    table_head,
    table_header,
    table_row,
};
use crate::shell::app_shell;

#[page]
pub(super) async fn status(cx: &Cx) -> Result<impl View> {
    let access = gate(cx, WebRole::Operator).await?;
    if access != Gate::Open {
        return Ok(view! { closed(role: WebRole::Operator, gate: access) }.boxed());
    }
    let rows = process_status(cx).await;

    Ok(view! { app_shell(status_page(rows: rows)) }.boxed())
}

#[component]
pub async fn status_page(
    rows: std::result::Result<Vec<ProcessView>, AdminError>,
) -> Result<impl View> {
    Ok(view! {
        <div class="page">
            <div class="page-header">
                <div>
                    <h1>"Bot status"</h1>
                    <p class="page-lead">
                        "Each bot and the worker report every minute."
                    </p>
                </div>
            </div>
            match rows {
                Err(e) => alert(
                    variant: AlertVariant::Destructive,
                    attrs: attributes! { class="error" },
                    alert_title(
                        "Couldn't load process status: "
                        (e.to_string())
                    )
                ),
                Ok(rows) => if rows.is_empty() {
                    <p class="text-text-2">"No process has reported yet."</p>
                } else {
                    table(
                        table_header(
                            table_row(
                                table_head("Process")
                                table_head("Build")
                                table_head("Started")
                                table_head("Last seen")
                                table_head("State")
                            )
                        )
                        table_body(
                            #[key(row.process.as_str())]
                            for row in &rows {
                                table_row(
                                    table_cell((row.label.as_str()))
                                    table_cell(<span class="mono">(row.build.as_str())</span>)
                                    table_cell((row.started.as_str()))
                                    table_cell((row.last_seen.as_str()))
                                    table_cell(
                                        if row.live {
                                            badge(variant: BadgeVariant::Primary, "Live")
                                        } else {
                                            badge(variant: BadgeVariant::Destructive, "Stale")
                                        }
                                    )
                                )
                            }
                        )
                    )
                },
            }
        </div>
    })
}
