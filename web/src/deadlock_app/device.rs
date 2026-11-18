use deadlock_protocol::MAX_TEXT_BYTES;
use deadlock_protocol::auth::{AccessToken, DevicePoll};
use rand::RngExt;
use sqlx::PgPool;

use super::{AppApiError, auth, tokens};

pub const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
pub const USER_CODE_LEN: usize = 8;
pub const EXPIRES_IN_SECS: u32 = 600;
pub const INTERVAL_SECS: u32 = 5;
pub const SLOW_DOWN_STEP_SECS: u32 = 5;
pub const MAX_INTERVAL_SECS: u32 = 60;
const INSERT_ATTEMPTS: usize = 3;

pub struct PendingRequest {
    pub user_code: String,
    pub device_name: String,
    pub app_version: String,
    pub requested_at: jiff::Timestamp,
}

pub struct IssuedCode {
    pub device_code: String,
    pub user_code: String,
}

#[must_use]
pub fn new_user_code() -> String {
    let mut rng = rand::rng();
    (0..USER_CODE_LEN)
        .filter_map(|_| {
            USER_CODE_ALPHABET
                .get(rng.random_range(0..USER_CODE_ALPHABET.len()))
                .copied()
                .map(char::from)
        })
        .collect()
}

#[must_use]
pub fn normalize_user_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == USER_CODE_LEN
        && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)))
    .then_some(code)
}

#[must_use]
pub fn display_user_code(code: &str) -> String {
    format!(
        "{}-{}",
        code.get(..4).unwrap_or_default(),
        code.get(4..).unwrap_or_default()
    )
}

#[must_use]
pub fn clean_text(input: &str) -> Option<String> {
    let stripped: String = input.chars().filter(|c| !c.is_control()).collect();
    let text = stripped.trim();
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cleaned = text.get(..end).unwrap_or(text).trim_end();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

pub async fn issue(
    pool: &PgPool,
    device_name: &str,
    app_version: &str,
) -> sqlx::Result<IssuedCode> {
    let mut attempt = 1;
    loop {
        let device_code = auth::new_secret();
        let user_code = new_user_code();
        let result = sqlx::query!(
            r#"
            INSERT INTO deadlock_device_codes
                (device_code_hash, user_code, device_name, app_version, expires_at, interval_secs)
            VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6)
            "#,
            &auth::hash_secret(&device_code)[..],
            user_code,
            device_name,
            app_version,
            f64::from(EXPIRES_IN_SECS),
            INTERVAL_SECS.cast_signed()
        )
        .execute(pool)
        .await;

        match result {
            Ok(_) => return Ok(IssuedCode { device_code, user_code }),
            Err(sqlx::Error::Database(e))
                if e.is_unique_violation() && attempt < INSERT_ATTEMPTS =>
            {
                attempt += 1;
            },
            Err(e) => return Err(e),
        }
    }
}

pub async fn poll(
    pool: &PgPool,
    device_code: &str,
) -> Result<DevicePoll, AppApiError> {
    if !auth::is_well_formed(device_code) {
        return Ok(DevicePoll::Expired);
    }
    let hash = auth::hash_secret(device_code);

    let mut tx = pool.begin().await?;
    let Some(row) = sqlx::query!(
        r#"
        SELECT expires_at <= now() AS "expired!",
               granted_at IS NOT NULL AS "granted!",
               denied_at IS NOT NULL AS "denied!",
               approved_by,
               interval_secs AS "interval_secs!",
               (last_polled_at IS NOT NULL
                AND last_polled_at > now() - make_interval(secs => interval_secs::double precision)) AS "early!",
               device_name,
               app_version
        FROM deadlock_device_codes
        WHERE device_code_hash = $1
        FOR UPDATE
        "#,
        &hash[..]
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(DevicePoll::Expired);
    };

    if row.granted || row.expired {
        return Ok(DevicePoll::Expired);
    }
    if row.denied {
        return Ok(DevicePoll::Denied);
    }

    if let Some(discord_id) = row.approved_by {
        sqlx::query!(
            "UPDATE deadlock_device_codes SET granted_at = now(), last_polled_at = now() WHERE device_code_hash = $1",
            &hash[..]
        )
        .execute(&mut *tx)
        .await?;
        let secret = auth::new_secret();
        tokens::insert(
            &mut tx,
            &auth::hash_secret(&secret),
            discord_id,
            &row.device_name,
            &row.app_version,
        )
        .await?;
        tx.commit().await?;
        return Ok(DevicePoll::Granted { access_token: AccessToken::new(secret) });
    }

    let interval = u32::try_from(row.interval_secs).unwrap_or(INTERVAL_SECS);
    if row.early {
        let next = (interval + SLOW_DOWN_STEP_SECS).min(MAX_INTERVAL_SECS);
        sqlx::query!(
            "UPDATE deadlock_device_codes SET interval_secs = $2, last_polled_at = now() WHERE device_code_hash = $1",
            &hash[..],
            next.cast_signed()
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(DevicePoll::SlowDown { interval_secs: next });
    }

    sqlx::query!(
        "UPDATE deadlock_device_codes SET last_polled_at = now() WHERE device_code_hash = $1",
        &hash[..]
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(DevicePoll::Pending)
}

pub async fn find_pending(
    pool: &PgPool,
    user_code: &str,
) -> sqlx::Result<Option<PendingRequest>> {
    let row = sqlx::query!(
        r#"
        SELECT user_code, device_name, app_version,
               requested_at AS "requested_at: jiff_sqlx::Timestamp"
        FROM deadlock_device_codes
        WHERE user_code = $1 AND expires_at > now()
          AND approved_at IS NULL AND denied_at IS NULL
        "#,
        user_code
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| PendingRequest {
        user_code: row.user_code,
        device_name: row.device_name,
        app_version: row.app_version,
        requested_at: row.requested_at.to_jiff(),
    }))
}

pub async fn approve(
    pool: &PgPool,
    user_code: &str,
    discord_id: i64,
) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE deadlock_device_codes SET approved_by = $2, approved_at = now()
        WHERE user_code = $1 AND expires_at > now()
          AND approved_at IS NULL AND denied_at IS NULL
        "#,
        user_code,
        discord_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

pub async fn prune_expired(pool: &PgPool) -> sqlx::Result<u64> {
    let result = sqlx::query!(
        "DELETE FROM deadlock_device_codes WHERE expires_at <= now() - interval '1 day'"
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

pub async fn deny(pool: &PgPool, user_code: &str) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE deadlock_device_codes SET denied_at = now()
        WHERE user_code = $1 AND expires_at > now()
          AND approved_at IS NULL AND denied_at IS NULL
        "#,
        user_code
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}
