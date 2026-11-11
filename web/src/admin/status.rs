use jiff::{SignedDuration, Timestamp};
use topcoat::context::Cx;
use zayden_app::applications::{ProcessRow, ProcessStatus};

use super::error::AdminError;
use crate::auth::app_state;

pub const STALE_AFTER: SignedDuration = SignedDuration::from_secs(150);

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M UTC";
const BUILD_LENGTH: usize = 7;
const WORKER: &str = "worker";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessView {
    pub process: String,
    pub label: String,
    pub build: String,
    pub started: String,
    pub last_seen: String,
    pub live: bool,
}

#[must_use]
pub fn process_view(row: &ProcessRow, now: Timestamp) -> ProcessView {
    let label = row.name.clone().unwrap_or_else(|| {
        if row.process == WORKER { "Worker".to_owned() } else { row.process.clone() }
    });

    ProcessView {
        process: row.process.clone(),
        label,
        build: row.build_sha.chars().take(BUILD_LENGTH).collect(),
        started: row.started_at.strftime(TIME_FORMAT).to_string(),
        last_seen: row.last_seen.strftime(TIME_FORMAT).to_string(),
        live: now.duration_since(row.last_seen) <= STALE_AFTER,
    }
}

pub async fn process_status(cx: &Cx) -> Result<Vec<ProcessView>, AdminError> {
    let app = app_state(cx)?;
    let rows = ProcessStatus::list(&app.db).await?;
    let now = Timestamp::now();

    Ok(rows.iter().map(|row| process_view(row, now)).collect())
}
