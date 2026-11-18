//! The Deadlock app API (device sign-in, token, `/me`), the `/link/app`
//! verification page and the `/account` additions, through the app router
//! against Postgres.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Debug, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use accounts::links::AccountLink;
use accounts::provider::Provider;
use deadlock::DeadlockProxy;
use deadlock::client::CacheKind;
use deadlock::db::{proxy_cache, report_waits, user_settings};
use deadlock::report_fetch::{cache_key, metadata_canonical, salts_canonical};
use deadlock_api::{DeadlockClient, Limiter};
use deadlock_protocol::auth::{
    DeviceCode,
    DevicePoll,
    DevicePollRequest,
    DeviceSecret,
    Me,
    Tier,
};
use deadlock_protocol::error::{ApiError, ErrorCode};
use deadlock_protocol::paths::{MATCHES, proxy_upstream};
use deadlock_protocol::rest::{MatchReport, MatchReportAck};
use http_body_util::BodyExt;
use jiff::{SignedDuration, Timestamp};
use secrecy::SecretString;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use topcoat::asset::{AssetBundle, RouterBuilderAssetExt};
use topcoat::router::request::Request;
use topcoat::router::response::Response;
use topcoat::router::{Body, Method, Router, StatusCode, header};
use web::components::brand::LOGO;
use web::deadlock_app::limits::{ClientKey, client_key};
use web::document::{PENDING_SUBMIT, STYLESHEET};
use web::state::{SessionIdentity, WebState};
use zayden_app::config::BotConfig;
use zayden_app::state::AppState as ZaydenAppState;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const MEMBER: &str = "session=member-token";
const OTHER: &str = "session=other-token";
const USER_ID: i64 = 41;
const OTHER_ID: i64 = 43;
const STEAM_ID: &str = "76561197960287930";
const FORM: &str = "application/x-www-form-urlencoded";
const PROTOCOL: (&str, &str) = ("x-deadlock-protocol", "1");
const ALPHABET: &str = "BCDFGHJKLMNPQRSTVWXZ";
const PROXY: &str = "/api/deadlock/v1/proxy";
const HEROES: &str = include_str!("fixtures/deadlock/heroes.json");

fn bundle_dir() -> TestResult<PathBuf> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| write_bundle().map_err(|e| e.to_string()))
        .clone()
        .map_err(Into::into)
}

fn write_bundle() -> TestResult<PathBuf> {
    let dir = std::env::temp_dir()
        .join(format!("web-deadlock-app-assets-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let assets = [
        ("tailwind-0123456789abcdef.css", "text/css", STYLESHEET.id().as_u64()),
        (
            "topcoat-0123456789abcdef.js",
            "text/javascript",
            topcoat::runtime::SCRIPT.id().as_u64(),
        ),
        (
            "pending-submit-0123456789abcdef.js",
            "text/javascript",
            PENDING_SUBMIT.id().as_u64(),
        ),
        ("logo-0123456789abcdef.png", "image/png", LOGO.id().as_u64()),
    ];
    let mut manifest = String::from("version = 1\n");
    for (file, content_type, id) in assets {
        std::fs::write(dir.join(file), "")?;
        write!(
            manifest,
            "\n[[assets]]\nid = {id}\nfile = \"{file}\"\nhash = \"0\"\ncontent_type = \"{content_type}\"\n"
        )?;
    }
    std::fs::write(dir.join("manifest.toml"), manifest)?;
    Ok(dir)
}

fn config() -> BotConfig {
    BotConfig {
        discord_token: SecretString::from("test-bot-token"),
        bungie_api_key: None,
        ai_provider_key: None,
        google_api_key: None,
        discord_client_secret: Some(SecretString::from("test-client-secret")),
        spotify: None,
        ai_api_endpoint: String::new(),
        ai_model: String::new(),
        ai_model_structured: String::new(),
        ai_model_pro: String::new(),
        bot_owner: 0,
        zayden_guild: 0,
        llamad2_guild: 0,
        zayden_id: 1,
        maria_id: None,
        auto_leave: false,
        error_log_webhook: None,
        normal_log_webhook: None,
        flaresolverr_url: None,
        deadlock_api_key: None,
        youtube_cookies: None,
        palworld_paldex_url: None,
        palworld_palcalc_url: None,
        palworld_save_dir: None,
        palworld_uploads_dir: PathBuf::new(),
        pelican: None,
        jellyfin: None,
        hosting: None,
        redirect_uri: "http://localhost:3000/auth/callback".to_owned(),
        dashboard_url: "http://localhost:3000".to_owned(),
        bind_addr: "127.0.0.1:0".to_owned(),
        invite_url: None,
        upgrade_url: None,
        kofi_verification_token: None,
        patreon: None,
        youtube: None,
        discord_sku_pro: None,
        discord_sku_ultra: None,
        radio_stations: Arc::from(Vec::new()),
    }
}

async fn harness(pool: &PgPool) -> TestResult<Router> {
    harness_with(pool, None).await
}

async fn harness_with(pool: &PgPool, upstream: Option<&str>) -> TestResult<Router> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let config = config();
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let mut state = WebState::new(app, &config)?;
    if let Some(base) = upstream {
        let client = DeadlockClient::builder()
            .base_url(base)
            .limiter(Limiter::with_configs([], Duration::ZERO))
            .build()?;
        state.deadlock_proxy =
            Arc::new(DeadlockProxy::from_client(client, pool.clone()));
    }
    for (token, user_id) in [("member-token", USER_ID), ("other-token", OTHER_ID)] {
        state
            .sessions
            .insert(token.to_owned(), SessionIdentity {
                user_id,
                access_token: format!("{token}-access"),
            })
            .await;
        state.discord.user_guilds.insert(user_id, Arc::from([])).await;
    }

    let base =
        web::router().assets(AssetBundle::load_dir(bundle_dir()?)?).with(state);
    Ok(base.build())
}

async fn api(
    router: &Router,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> TestResult<Response> {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_owned()));
    Ok(router.handle(request.body(body)?).await)
}

async fn get(
    router: &Router,
    path: &str,
    cookie: Option<&str>,
) -> TestResult<Response> {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    Ok(router.handle(request.body(Body::empty())?).await)
}

async fn post_form(
    router: &Router,
    path: &str,
    cookie: &str,
    body: &str,
) -> TestResult<Response> {
    let request = Request::post(path)
        .header(header::CONTENT_TYPE, FORM)
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_owned()))?;
    Ok(router.handle(request).await)
}

async fn body_text(response: Response) -> TestResult<String> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8(bytes.to_vec())?)
}

fn location(response: &Response) -> Option<&str> {
    response.headers().get(header::LOCATION).and_then(|v| v.to_str().ok())
}

fn set_cookies(response: &Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect()
}

async fn wire<T>(response: Response) -> TestResult<(StatusCode, T)>
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let status = response.status();
    let value: serde_json::Value =
        serde_json::from_str(&body_text(response).await?)?;
    let parsed: T = serde_json::from_value(value.clone())?;
    assert_eq!(serde_json::to_value(&parsed)?, value, "wire shape");
    Ok((status, parsed))
}

async fn start_named(router: &Router, name: &str) -> TestResult<DeviceCode> {
    let body = serde_json::json!({ "device_name": name, "app_version": "1.2.3" })
        .to_string();
    let response = api(
        router,
        Method::POST,
        "/api/deadlock/v1/device",
        &[PROTOCOL],
        Some(&body),
    )
    .await?;
    let (status, code) = wire::<DeviceCode>(response).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(code)
}

async fn start(router: &Router) -> TestResult<DeviceCode> {
    start_named(router, "Test Deck").await
}

async fn poll_code(router: &Router, code: &DeviceCode) -> TestResult<DevicePoll> {
    let body = serde_json::to_string(&DevicePollRequest {
        device_code: code.device_code.clone(),
    })?;
    let response = api(
        router,
        Method::POST,
        "/api/deadlock/v1/device/token",
        &[PROTOCOL],
        Some(&body),
    )
    .await?;
    let (status, poll) = wire::<DevicePoll>(response).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(poll)
}

async fn decide(
    router: &Router,
    cookie: &str,
    code: &DeviceCode,
    decision: &str,
) -> TestResult<Response> {
    post_form(
        router,
        "/link/app",
        cookie,
        &format!("code={}&decision={decision}", code.user_code),
    )
    .await
}

async fn link_steam(pool: &PgPool) -> TestResult {
    AccountLink::upsert_transfer(pool, USER_ID, Provider::Steam, STEAM_ID).await?;
    Ok(())
}

async fn grant_token(router: &Router, pool: &PgPool) -> TestResult<String> {
    link_steam(pool).await?;
    let code = start(router).await?;
    let response = decide(router, MEMBER, &code, "approve").await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let DevicePoll::Granted { access_token } = poll_code(router, &code).await?
    else {
        return Err("expected a grant".into());
    };
    Ok(access_token.expose().to_owned())
}

async fn me(router: &Router, authorization: Option<&str>) -> TestResult<Response> {
    let mut headers = vec![PROTOCOL];
    if let Some(value) = authorization {
        headers.push(("authorization", value));
    }
    api(router, Method::GET, "/api/deadlock/v1/me", &headers, None).await
}

fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn is_user_code(code: &str) -> bool {
    code.len() == 9
        && code.split_once('-').is_some_and(|(left, right)| {
            left.len() == 4
                && right.len() == 4
                && left.chars().chain(right.chars()).all(|c| ALPHABET.contains(c))
        })
}

fn api_error(error: &ApiError, code: ErrorCode) {
    assert_eq!(error.code, code, "{error:?}");
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn missing_protocol_header_is_426(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    let response =
        api(&router, Method::GET, "/api/deadlock/v1/me", &[], None).await.unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    api_error(&error, ErrorCode::ProtocolUnsupported);
    assert_eq!(error.retry_after_secs, None);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn wrong_protocol_version_is_426(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    let response = api(
        &router,
        Method::GET,
        "/api/deadlock/v1/me",
        &[("x-deadlock-protocol", "2")],
        None,
    )
    .await
    .unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    api_error(&error, ErrorCode::ProtocolUnsupported);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn unknown_api_path_is_json_404(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    let response =
        api(&router, Method::GET, "/api/deadlock/v1/nope", &[PROTOCOL], None)
            .await
            .unwrap();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map_or_else(String::new, str::to_owned);
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::NOT_FOUND);
    api_error(&error, ErrorCode::NotFound);
    assert!(content_type.starts_with("application/json"), "{content_type}");
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn device_start_issues_a_code(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    let code = start(&router).await.unwrap();

    assert!(is_user_code(&code.user_code), "{}", code.user_code);
    assert_eq!(code.verification_uri, "http://localhost:3000/link/app");
    assert_eq!(
        code.verification_uri_complete,
        format!("{}?code={}", code.verification_uri, code.user_code)
    );
    assert_eq!(code.expires_in_secs, 600);
    assert_eq!(code.interval_secs, 5);
    assert_eq!(code.device_code.expose().len(), 43);

    let row = sqlx::query!(
        "SELECT device_code_hash, user_code FROM deadlock_device_codes"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.device_code_hash, hash(code.device_code.expose()));
    assert_eq!(row.user_code, code.user_code.replace('-', ""));
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn device_start_rejects_bad_bodies(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    for body in [
        "not json".to_owned(),
        serde_json::json!({ "device_name": "   ", "app_version": "1" }).to_string(),
        serde_json::json!({ "device_name": "Deck", "app_version": "\u{7}" })
            .to_string(),
    ] {
        let response = api(
            &router,
            Method::POST,
            "/api/deadlock/v1/device",
            &[PROTOCOL],
            Some(&body),
        )
        .await
        .unwrap();
        let (status, error) = wire::<ApiError>(response).await.unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        api_error(&error, ErrorCode::BadRequest);
    }

    start_named(&router, &"a".repeat(200)).await.unwrap();
    let name = sqlx::query_scalar!("SELECT device_name FROM deadlock_device_codes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!name.is_empty() && name.len() <= 64, "{}", name.len());
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn device_flow_grants_once(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    link_steam(&pool).await.unwrap();
    let code = start(&router).await.unwrap();

    assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::Pending);
    assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::SlowDown {
        interval_secs: 10
    });

    let page = get(
        &router,
        &format!("/link/app?code={}", code.user_code.to_lowercase()),
        Some(MEMBER),
    )
    .await
    .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(body_text(page).await.unwrap().contains("Test Deck"));
    assert!(matches!(
        poll_code(&router, &code).await.unwrap(),
        DevicePoll::Pending | DevicePoll::SlowDown { .. }
    ));

    let response = decide(&router, MEMBER, &code, "approve").await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), Some("/link/app"));

    let DevicePoll::Granted { access_token } =
        poll_code(&router, &code).await.unwrap()
    else {
        panic!("expected a grant");
    };
    assert_eq!(access_token.expose().len(), 43);
    assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::Expired);

    let owner = sqlx::query_scalar!(
        "SELECT discord_id FROM deadlock_app_tokens WHERE token_hash = $1",
        &hash(access_token.expose())[..]
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner, USER_ID);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn unknown_device_code_is_expired(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    for secret in ["A".repeat(43), "short".to_owned()] {
        let code = DeviceCode {
            device_code: DeviceSecret::new(secret),
            user_code: String::new(),
            verification_uri: String::new(),
            verification_uri_complete: String::new(),
            expires_in_secs: 0,
            interval_secs: 0,
        };
        assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::Expired);
    }
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn denied_request_reports_denied(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    link_steam(&pool).await.unwrap();
    let code = start(&router).await.unwrap();

    let response = decide(&router, MEMBER, &code, "deny").await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::Denied);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn approval_needs_a_steam_link(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let code = start(&router).await.unwrap();

    let page =
        get(&router, &format!("/link/app?code={}", code.user_code), Some(MEMBER))
            .await
            .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(body_text(page).await.unwrap().contains("/account/link/steam"));

    let response = decide(&router, MEMBER, &code, "approve").await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::Pending);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn link_page_escapes_device_text(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    link_steam(&pool).await.unwrap();
    let code = start_named(&router, "<b>Deck</b>").await.unwrap();

    let page =
        get(&router, &format!("/link/app?code={}", code.user_code), Some(MEMBER))
            .await
            .unwrap();
    let html = body_text(page).await.unwrap();

    assert!(html.contains("&lt;b&gt;Deck&lt;/b&gt;"), "{html}");
    assert!(!html.contains("<b>Deck</b>"), "{html}");
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn signed_out_link_page_remembers_the_code(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    let response = get(&router, "/link/app?code=bcdf-ghjk", None).await.unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), Some("/auth/discord"));
    let cookies = set_cookies(&response);
    assert!(
        cookies.iter().any(|cookie| {
            cookie.starts_with("login_next=")
                && cookie
                    .replace("%2F", "/")
                    .replace("%3F", "?")
                    .replace("%3D", "=")
                    .contains("/link/app?code=BCDFGHJK")
        }),
        "{cookies:?}"
    );
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn me_returns_account_and_settings(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let bearer = format!("Bearer {token}");

    let (status, body) =
        wire::<Me>(me(&router, Some(&bearer)).await.unwrap()).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.discord_user_id, "41");
    assert_eq!(body.account_id, Some(22202));
    assert_eq!(body.tier, Tier::Free);
    assert!(
        body.settings.match_fetch
            && body.settings.dm_summaries
            && body.settings.patch_briefing
    );

    user_settings::set_match_fetch(&pool, USER_ID, false, None).await.unwrap();
    let (_, body) =
        wire::<Me>(me(&router, Some(&bearer)).await.unwrap()).await.unwrap();
    assert!(!body.settings.match_fetch);

    let last_used = sqlx::query_scalar!(
        r#"SELECT last_used_at IS NOT NULL AS "used!" FROM deadlock_app_tokens"#
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(last_used);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn me_requires_a_valid_bearer(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let basic = format!("Basic {token}");

    for authorization in [None, Some("Bearer abc"), Some(basic.as_str())] {
        let (status, error) =
            wire::<ApiError>(me(&router, authorization).await.unwrap())
                .await
                .unwrap();

        assert_eq!(status, StatusCode::UNAUTHORIZED, "{authorization:?}");
        api_error(&error, ErrorCode::Unauthorized);
    }

    let response = api(
        &router,
        Method::GET,
        "/api/deadlock/v1/me",
        &[PROTOCOL, ("cookie", MEMBER)],
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn delete_token_revokes_it(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let bearer = format!("Bearer {token}");

    let response = api(
        &router,
        Method::DELETE,
        "/api/deadlock/v1/token",
        &[PROTOCOL, ("authorization", &bearer)],
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(body_text(response).await.unwrap(), "");

    let response = me(&router, Some(&bearer)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn device_start_is_rate_limited(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    for _ in 0..10 {
        start(&router).await.unwrap();
    }

    let body =
        serde_json::json!({ "device_name": "Deck", "app_version": "1" }).to_string();
    let response = api(
        &router,
        Method::POST,
        "/api/deadlock/v1/device",
        &[PROTOCOL],
        Some(&body),
    )
    .await
    .unwrap();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    api_error(&error, ErrorCode::RateLimited);
    assert!(retry_after >= 1);
    assert_eq!(error.retry_after_secs, Some(retry_after));
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn code_entry_is_rate_limited(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    link_steam(&pool).await.unwrap();

    for _ in 0..10 {
        let page =
            get(&router, "/link/app?code=BCDFGHJK", Some(MEMBER)).await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert!(body_text(page).await.unwrap().contains("invalid or has expired"));
    }

    let page = get(&router, "/link/app?code=BCDFGHJK", Some(MEMBER)).await.unwrap();
    assert!(body_text(page).await.unwrap().contains("Too many attempts"));
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn account_lists_and_revokes_devices(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let bearer = format!("Bearer {token}");
    let id = sqlx::query_scalar!("SELECT id FROM deadlock_app_tokens")
        .fetch_one(&pool)
        .await
        .unwrap();
    let path = format!("/account/devices/{id}/revoke");

    let page = get(&router, "/account", Some(MEMBER)).await.unwrap();
    let html = body_text(page).await.unwrap();
    assert!(html.contains("Test Deck"), "{html}");
    assert!(html.contains(&format!(r#"action="{path}""#)), "{html}");

    let response = post_form(&router, &path, OTHER, "").await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(me(&router, Some(&bearer)).await.unwrap().status(), StatusCode::OK);

    let response = post_form(&router, &path, MEMBER, "").await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        me(&router, Some(&bearer)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let page = get(&router, "/account", Some(MEMBER)).await.unwrap();
    assert!(body_text(page).await.unwrap().contains("No apps are signed in."));
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn account_saves_match_fetch(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    user_settings::set_match_fetch(&pool, USER_ID, false, None).await.unwrap();

    let response = post_form(&router, "/account/deadlock", MEMBER, "match_fetch=on")
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(user_settings::match_fetch(&pool, USER_ID).await.unwrap());

    let response =
        post_form(&router, "/account/deadlock", MEMBER, "dm_summaries=on")
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(!user_settings::match_fetch(&pool, USER_ID).await.unwrap());

    let page = get(&router, "/account", Some(MEMBER)).await.unwrap();
    assert!(body_text(page).await.unwrap().contains(r#"name="match_fetch""#));
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn expired_code_cannot_be_approved(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    link_steam(&pool).await.unwrap();
    let code = start(&router).await.unwrap();
    sqlx::query!(
        "UPDATE deadlock_device_codes SET expires_at = now() - interval '1 second'"
    )
    .execute(&pool)
    .await
    .unwrap();

    let page =
        get(&router, &format!("/link/app?code={}", code.user_code), Some(MEMBER))
            .await
            .unwrap();
    assert!(body_text(page).await.unwrap().contains("invalid or has expired"));

    let response = decide(&router, MEMBER, &code, "approve").await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(poll_code(&router, &code).await.unwrap(), DevicePoll::Expired);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn wrong_method_is_json_400(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    for (method, path) in [
        (Method::GET, "/api/deadlock/v1/device"),
        (Method::POST, "/api/deadlock/v1/me"),
    ] {
        let response = api(&router, method, path, &[PROTOCOL], None).await.unwrap();
        let (status, error) = wire::<ApiError>(response).await.unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        api_error(&error, ErrorCode::BadRequest);
    }

    let response = api(&router, Method::GET, "/api/deadlock/v1", &[PROTOCOL], None)
        .await
        .unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND);
    api_error(&error, ErrorCode::NotFound);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn prune_removes_day_old_codes(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let old = start(&router).await.unwrap();
    sqlx::query!(
        "UPDATE deadlock_device_codes SET expires_at = now() - interval '2 days' WHERE user_code = $1",
        old.user_code.replace('-', "")
    )
    .execute(&pool)
    .await
    .unwrap();
    let kept = start(&router).await.unwrap();

    web::session_pruning::prune_expired_device_codes(&pool).await;

    let remaining =
        sqlx::query_scalar!("SELECT user_code FROM deadlock_device_codes")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, vec![kept.user_code.replace('-', "")]);
}

#[test]
fn client_key_groups_ipv6_by_64_bit_prefix() {
    let key = |text: &str| client_key(Some(text.parse().unwrap()));

    assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff::9"));
    assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
    assert_eq!(key("::ffff:192.0.2.7"), key("192.0.2.7"));
    assert_eq!(client_key(None), ClientKey::Unknown);
}

type Requests = Arc<Mutex<Vec<String>>>;
type Routes = Arc<Mutex<HashMap<String, Reply>>>;

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn new(status: u16, body: &str) -> Self {
        Self { status, body: body.to_owned() }
    }

    fn render(&self) -> String {
        format!(
            "HTTP/1.1 {} Mock\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            self.status,
            self.body.len(),
            self.body
        )
    }
}

struct Mock {
    base: String,
    requests: Requests,
    routes: Routes,
}

impl Mock {
    fn serve(&self, path: &str, reply: Reply) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.insert(path.to_owned(), reply);
        }
    }

    fn count(&self) -> usize {
        self.requests.lock().map_or(0, |requests| requests.len())
    }

    fn heads(&self) -> Vec<String> {
        self.requests.lock().map_or_else(|_| Vec::new(), |requests| requests.clone())
    }
}

async fn read_head(socket: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        let n = match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => n,
        };
        buf.extend_from_slice(chunk.get(..n)?);

        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&buf).into_owned());
        }
    }
}

fn path_of(head: &str) -> &str {
    head.split_whitespace()
        .nth(1)
        .and_then(|target| target.split('?').next())
        .unwrap_or_default()
}

async fn spawn_mock(fallback: Reply) -> Option<Mock> {
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let base = format!("http://{}", listener.local_addr().ok()?);
    let requests = Requests::default();
    let routes = Routes::default();
    let recorder = Arc::clone(&requests);
    let table = Arc::clone(&routes);
    let fallback = fallback.render();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };

            let head = read_head(&mut socket).await;
            let path = head.as_deref().map_or("", path_of).to_owned();

            if let Some(head) = head
                && let Ok(mut recorded) = recorder.lock()
            {
                recorded.push(head);
            }

            let response = table
                .lock()
                .ok()
                .and_then(|table| table.get(&path).map(Reply::render))
                .unwrap_or_else(|| fallback.clone());

            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    Some(Mock { base, requests, routes })
}

async fn proxy_harness(pool: &PgPool) -> TestResult<(Router, Mock)> {
    let mock = spawn_mock(Reply::new(200, "[]")).await.ok_or("mock failed")?;
    let router = harness_with(pool, Some(&mock.base)).await?;
    Ok((router, mock))
}

async fn proxy_get(
    router: &Router,
    upstream: &str,
    token: Option<&str>,
) -> TestResult<Response> {
    let bearer = token.map(|token| format!("Bearer {token}"));
    let mut headers = vec![PROTOCOL];
    if let Some(bearer) = bearer.as_deref() {
        headers.push(("authorization", bearer));
    }
    api(router, Method::GET, &format!("{PROXY}{upstream}"), &headers, None).await
}

fn header_text(response: &Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn proxy_serves_the_upstream_body_verbatim(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    mock.serve("/v1/assets/heroes", Reply::new(200, HEROES));
    let token = grant_token(&router, &pool).await.unwrap();

    let response =
        proxy_get(&router, "/v1/assets/heroes?client_version=6123", Some(&token))
            .await
            .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, "content-type").as_deref(),
        Some("application/json")
    );
    let fetched_at: i64 =
        header_text(&response, "x-deadlock-fetched-at").unwrap().parse().unwrap();
    assert!((Timestamp::now().as_millisecond() - fetched_at).abs() < 60_000);
    assert_eq!(header_text(&response, "x-deadlock-stale"), None);
    assert_eq!(body_text(response).await.unwrap(), HEROES);
    let heads = mock.heads();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with("GET /v1/assets/heroes?client_version=6123 HTTP/1.1"),
        "{}",
        heads[0]
    );
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn proxy_canonicalizes_the_request(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();

    let first = proxy_get(
        &router,
        "/v1/analytics/item-flow-stats?hero_ids=1&game_mode=normal",
        Some(&token),
    )
    .await
    .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = body_text(first).await.unwrap();
    let second = proxy_get(
        &router,
        "/v1/analytics/item-flow-stats?game_mode=normal&hero_ids=1",
        Some(&token),
    )
    .await
    .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    assert_eq!(body_text(second).await.unwrap(), first);
    let heads = mock.heads();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with(
            "GET /v1/analytics/item-flow-stats?game_mode=normal&hero_ids=1 HTTP/1.1"
        ),
        "{}",
        heads[0]
    );
    let row = proxy_cache::get(
        &pool,
        "proxy:/v1/analytics/item-flow-stats?game_mode=normal&hero_ids=1",
    )
    .await
    .unwrap();
    assert!(row.is_some());
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn proxy_rejects_unlisted_paths(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();

    for upstream in [
        "/v1/players/1/match-history",
        "/v1/matches/1/metadata",
        "/v1/matches/1/salts?disable_steam=false",
        "/v1/assets/he%2Froes",
        "/v1/analytics/item-flow-stats?hero_ids=%31",
        "/v1/analytics/item-flow-stats?hero_ids=1&x=2",
    ] {
        let response = proxy_get(&router, upstream, Some(&token)).await.unwrap();
        let (status, error) = wire::<ApiError>(response).await.unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST, "{upstream}");
        api_error(&error, ErrorCode::NotProxied);
    }
    assert_eq!(mock.count(), 0);

    let response = proxy_get(&router, "", Some(&token)).await.unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND);
    api_error(&error, ErrorCode::NotFound);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn proxy_requires_a_token_and_protocol(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();

    let response = proxy_get(&router, "/v1/assets/heroes", None).await.unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    api_error(&error, ErrorCode::Unauthorized);

    let response =
        api(&router, Method::GET, &format!("{PROXY}/v1/assets/heroes"), &[], None)
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(mock.count(), 0);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn upstream_not_found_is_404_and_not_cached(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    mock.serve("/v1/matches/1/metadata", Reply::new(404, "{}"));
    let token = grant_token(&router, &pool).await.unwrap();
    let upstream = "/v1/matches/1/metadata?disable_steam=true";

    for _ in 0..2 {
        let response = proxy_get(&router, upstream, Some(&token)).await.unwrap();
        let (status, error) = wire::<ApiError>(response).await.unwrap();

        assert_eq!(status, StatusCode::NOT_FOUND);
        api_error(&error, ErrorCode::NotFound);
    }

    assert_eq!(mock.count(), 2);
    let row = proxy_cache::get(&pool, &format!("proxy:{upstream}")).await.unwrap();
    assert!(row.is_none());
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn upstream_client_error_is_400(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    mock.serve("/v1/assets/heroes", Reply::new(400, "{}"));
    let token = grant_token(&router, &pool).await.unwrap();

    let response =
        proxy_get(&router, "/v1/assets/heroes", Some(&token)).await.unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    api_error(&error, ErrorCode::BadRequest);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn upstream_failure_is_502(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    mock.serve("/v1/assets/heroes", Reply::new(500, "{}"));
    mock.serve("/v1/assets/items", Reply::new(429, "{}"));
    let token = grant_token(&router, &pool).await.unwrap();

    for upstream in ["/v1/assets/heroes", "/v1/assets/items"] {
        let response = proxy_get(&router, upstream, Some(&token)).await.unwrap();
        let (status, error) = wire::<ApiError>(response).await.unwrap();

        assert_eq!(status, StatusCode::BAD_GATEWAY, "{upstream}");
        api_error(&error, ErrorCode::Upstream);
    }
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn stale_row_is_served_with_the_stale_header(pool: PgPool) {
    let (router, mock) = proxy_harness(&pool).await.unwrap();
    mock.serve("/v1/assets/heroes", Reply::new(503, "{}"));
    let token = grant_token(&router, &pool).await.unwrap();
    let fetched_at = proxy_cache::put(
        &pool,
        "proxy:/v1/assets/heroes",
        CacheKind::Asset,
        None,
        "[1]",
        Duration::ZERO,
    )
    .await
    .unwrap();

    let response =
        proxy_get(&router, "/v1/assets/heroes", Some(&token)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_text(&response, "x-deadlock-stale").as_deref(), Some("1"));
    assert_eq!(
        header_text(&response, "x-deadlock-fetched-at"),
        Some(fetched_at.as_millisecond().to_string())
    );
    assert_eq!(body_text(response).await.unwrap(), "[1]");
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn proxy_misses_are_rate_limited_per_token(pool: PgPool) {
    let (router, _mock) = proxy_harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let miss = |n: u32| format!("/v1/analytics/hero-build-stats/{n}");

    for n in 1..=20 {
        let response = proxy_get(&router, &miss(n), Some(&token)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{n}");
    }

    let mut limited = None;
    for n in 21..=25 {
        let response = proxy_get(&router, &miss(n), Some(&token)).await.unwrap();
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(response);
            break;
        }
    }
    let response = limited.unwrap();
    let retry_after = header_text(&response, "retry-after")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    api_error(&error, ErrorCode::RateLimited);
    assert!(retry_after >= 1);
    assert_eq!(error.retry_after_secs, Some(retry_after));

    let hit = proxy_get(&router, &miss(1), Some(&token)).await.unwrap();
    assert_eq!(hit.status(), StatusCode::OK);

    let other = grant_token(&router, &pool).await.unwrap();
    let response = proxy_get(&router, &miss(30), Some(&other)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

const REPORTED_MATCH: u64 = 136_219_028;
const REPORTED_ACCOUNT: u32 = 22_202;

async fn report(
    router: &Router,
    token: Option<&str>,
    body: &str,
) -> TestResult<Response> {
    let bearer = token.map(|token| format!("Bearer {token}"));
    let mut headers = vec![PROTOCOL];
    if let Some(bearer) = bearer.as_deref() {
        headers.push(("authorization", bearer));
    }
    api(router, Method::POST, MATCHES, &headers, Some(body)).await
}

fn report_body(
    account_id: u32,
    match_id: u64,
    ended_at_ms: i64,
) -> TestResult<String> {
    Ok(serde_json::to_string(&MatchReport {
        match_id,
        account_id,
        hero: Some("hero_inferno".to_owned()),
        ended_at_ms,
        client_version: Some(6123),
        party_size: None,
    })?)
}

fn minutes_ago_ms(minutes: i64) -> i64 {
    Timestamp::now().as_millisecond() - minutes * 60_000
}

async fn report_ack(response: Response) -> TestResult<(StatusCode, String)> {
    let status = response.status();
    Ok((status, body_text(response).await?))
}

async fn report_error(response: Response) -> TestResult<(StatusCode, ApiError)> {
    let (status, error) = wire::<ApiError>(response).await?;
    assert_eq!(error.retry_after_secs, None, "{error:?}");
    Ok((status, error))
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn report_accepts_a_new_match(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let ended_at_ms = minutes_ago_ms(10);
    let body = report_body(REPORTED_ACCOUNT, REPORTED_MATCH, ended_at_ms).unwrap();

    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, text) = report_ack(response).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, r#"{"status":"accepted"}"#);
    assert_eq!(
        serde_json::from_str::<MatchReportAck>(&text).unwrap(),
        MatchReportAck::Accepted
    );

    let row = report_waits::get(&pool, REPORTED_ACCOUNT, REPORTED_MATCH)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.discord_id, USER_ID);
    assert_eq!(row.ended_at.as_millisecond(), ended_at_ms);
    assert_eq!(row.expires_at, row.reported_at + SignedDuration::from_hours(24));
    assert_eq!(
        row.next_attempt_at,
        (row.ended_at + SignedDuration::from_mins(1)).max(row.reported_at)
    );
    assert_eq!(row.attempts, 0);
    assert_eq!(row.finished_at, None);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn repeat_report_is_known(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let body =
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    report(&router, Some(&token), &body).await.unwrap();
    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, text) = report_ack(response).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, r#"{"status":"known"}"#);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn cached_metadata_is_known(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    proxy_cache::put(
        &pool,
        &cache_key(&metadata_canonical(REPORTED_MATCH)),
        CacheKind::Match,
        None,
        "{\"match_info\":{}}",
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    let body =
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, text) = report_ack(response).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, r#"{"status":"known"}"#);
    let row = report_waits::get(&pool, REPORTED_ACCOUNT, REPORTED_MATCH)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.next_attempt_at, row.reported_at);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn unusable_cached_metadata_is_not_known(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    proxy_cache::put(
        &pool,
        &cache_key(&metadata_canonical(REPORTED_MATCH)),
        CacheKind::Match,
        None,
        "{}",
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    let body =
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, text) = report_ack(response).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, r#"{"status":"accepted"}"#);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn opted_out_report_inserts_nothing(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    user_settings::set_match_fetch(&pool, USER_ID, false, None).await.unwrap();
    let body =
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, text) = report_ack(response).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, r#"{"status":"opted_out"}"#);
    assert!(
        report_waits::get(&pool, REPORTED_ACCOUNT, REPORTED_MATCH)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn report_for_another_account_is_403(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let body = report_body(7, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, error) = report_error(response).await.unwrap();

    assert_eq!(status, StatusCode::FORBIDDEN);
    api_error(&error, ErrorCode::AccountMismatch);
    assert!(
        report_waits::get(&pool, 7, REPORTED_MATCH).await.unwrap().is_none()
            && report_waits::get(&pool, REPORTED_ACCOUNT, REPORTED_MATCH)
                .await
                .unwrap()
                .is_none()
    );
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn report_without_steam_link_is_403(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    AccountLink::delete(&pool, USER_ID, Provider::Steam).await.unwrap();
    let body =
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    let response = report(&router, Some(&token), &body).await.unwrap();
    let (status, error) = report_error(response).await.unwrap();

    assert_eq!(status, StatusCode::FORBIDDEN);
    api_error(&error, ErrorCode::NotLinked);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn report_rejects_bad_input(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let recent = minutes_ago_ms(10);

    for body in [
        report_body(REPORTED_ACCOUNT, 9_223_372_036_854_775_808, recent).unwrap(),
        report_body(REPORTED_ACCOUNT, 0, recent).unwrap(),
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(8 * 24 * 60))
            .unwrap(),
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(-60)).unwrap(),
        "{".to_owned(),
    ] {
        let response = report(&router, Some(&token), &body).await.unwrap();
        let (status, error) = report_error(response).await.unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        api_error(&error, ErrorCode::BadRequest);
    }
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn report_needs_a_token_and_protocol(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let body =
        report_body(REPORTED_ACCOUNT, REPORTED_MATCH, minutes_ago_ms(10)).unwrap();

    let response = report(&router, None, &body).await.unwrap();
    let (status, error) = report_error(response).await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    api_error(&error, ErrorCode::Unauthorized);

    let response =
        api(&router, Method::POST, MATCHES, &[], Some(&body)).await.unwrap();
    let (status, error) = report_error(response).await.unwrap();
    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    api_error(&error, ErrorCode::ProtocolUnsupported);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn reports_are_rate_limited_per_token(pool: PgPool) {
    let router = harness(&pool).await.unwrap();
    let token = grant_token(&router, &pool).await.unwrap();
    let ended_at_ms = minutes_ago_ms(10);

    for match_id in 1..=30 {
        let body = report_body(REPORTED_ACCOUNT, match_id, ended_at_ms).unwrap();
        let response = report(&router, Some(&token), &body).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{match_id}");
    }

    let body = report_body(REPORTED_ACCOUNT, 31, ended_at_ms).unwrap();
    let response = report(&router, Some(&token), &body).await.unwrap();
    let retry_after = header_text(&response, "retry-after")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    api_error(&error, ErrorCode::RateLimited);
    assert!(retry_after >= 1);
    assert_eq!(error.retry_after_secs, Some(retry_after));

    let other = grant_token(&router, &pool).await.unwrap();
    let body = report_body(REPORTED_ACCOUNT, 32, ended_at_ms).unwrap();
    let response = report(&router, Some(&other), &body).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn report_wrong_method_is_json_400(pool: PgPool) {
    let router = harness(&pool).await.unwrap();

    let response =
        api(&router, Method::GET, MATCHES, &[PROTOCOL], None).await.unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    api_error(&error, ErrorCode::BadRequest);
}

#[test]
fn report_keys_match_the_proxy_allowlist() {
    for canonical in
        [metadata_canonical(REPORTED_MATCH), salts_canonical(REPORTED_MATCH)]
    {
        assert_eq!(proxy_upstream(&canonical), Some(canonical));
    }
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn proxy_wrong_method_is_json_400(pool: PgPool) {
    let (router, _mock) = proxy_harness(&pool).await.unwrap();

    let response = api(
        &router,
        Method::POST,
        &format!("{PROXY}/v1/assets/heroes"),
        &[PROTOCOL],
        None,
    )
    .await
    .unwrap();
    let (status, error) = wire::<ApiError>(response).await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    api_error(&error, ErrorCode::BadRequest);
}
