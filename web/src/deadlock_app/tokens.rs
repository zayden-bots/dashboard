use sqlx::PgPool;

pub struct LiveToken {
    pub id: i64,
    pub discord_id: i64,
    pub stale: bool,
}

pub struct AppDevice {
    pub id: i64,
    pub device_name: String,
    pub app_version: String,
    pub created_at: jiff::Timestamp,
    pub last_used_at: Option<jiff::Timestamp>,
}

pub async fn find_live(
    pool: &PgPool,
    hash: &[u8],
) -> sqlx::Result<Option<LiveToken>> {
    sqlx::query_as!(
        LiveToken,
        r#"
        SELECT id, discord_id,
               (last_used_at IS NULL OR last_used_at < now() - interval '1 minute') AS "stale!"
        FROM deadlock_app_tokens
        WHERE token_hash = $1 AND revoked_at IS NULL
        "#,
        hash
    )
    .fetch_optional(pool)
    .await
}

pub async fn touch(pool: &PgPool, id: i64) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE deadlock_app_tokens SET last_used_at = now() WHERE id = $1",
        id
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert(
    conn: &mut sqlx::PgConnection,
    hash: &[u8],
    discord_id: i64,
    device_name: &str,
    app_version: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO deadlock_app_tokens (token_hash, discord_id, device_name, app_version)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
        hash,
        discord_id,
        device_name,
        app_version
    )
    .fetch_one(conn)
    .await
}

pub async fn revoke(pool: &PgPool, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE deadlock_app_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

pub async fn revoke_owned(
    pool: &PgPool,
    id: i64,
    discord_id: i64,
) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE deadlock_app_tokens SET revoked_at = now()
        WHERE id = $1 AND discord_id = $2 AND revoked_at IS NULL
        "#,
        id,
        discord_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

pub async fn list_live(
    pool: &PgPool,
    discord_id: i64,
) -> sqlx::Result<Vec<AppDevice>> {
    let rows = sqlx::query!(
        r#"
        SELECT id, device_name, app_version,
               created_at AS "created_at: jiff_sqlx::Timestamp",
               last_used_at AS "last_used_at: jiff_sqlx::Timestamp"
        FROM deadlock_app_tokens
        WHERE discord_id = $1 AND revoked_at IS NULL
        ORDER BY created_at DESC, id DESC
        "#,
        discord_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| AppDevice {
            id: row.id,
            device_name: row.device_name,
            app_version: row.app_version,
            created_at: row.created_at.to_jiff(),
            last_used_at: row.last_used_at.map(jiff_sqlx::Timestamp::to_jiff),
        })
        .collect())
}
