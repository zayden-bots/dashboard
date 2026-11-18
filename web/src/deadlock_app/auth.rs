use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use sha2::{Digest, Sha256};
use topcoat::context::Cx;
use topcoat::router::request::headers;
use topcoat::router::{HeaderMap, header};

use super::{AppApiError, tokens};
use crate::auth::db_pool;

pub const SECRET_LEN: usize = 43;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppCaller {
    pub token_id: i64,
    pub discord_id: i64,
}

#[must_use]
pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes[..]);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[must_use]
pub fn is_well_formed(secret: &str) -> bool {
    secret.len() == SECRET_LEN
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[must_use]
pub fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[must_use]
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && is_well_formed(token)).then_some(token)
}

pub async fn authenticate(cx: &Cx) -> Result<AppCaller, AppApiError> {
    let token = bearer_token(headers(cx)).ok_or(AppApiError::Unauthorized)?;
    let pool = db_pool(cx)?;
    let live = tokens::find_live(pool, &hash_secret(token))
        .await?
        .ok_or(AppApiError::Unauthorized)?;
    if live.stale {
        tokens::touch(pool, live.id).await?;
    }
    Ok(AppCaller { token_id: live.id, discord_id: live.discord_id })
}
