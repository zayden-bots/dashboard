//! The Deadlock app API (device sign-in, token, `/me`), the `/link/app`
//! verification page and the `/account` additions, through the app router
//! against Postgres.

use std::error::Error;
use std::fmt::{Debug, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use accounts::links::AccountLink;
use accounts::provider::Provider;
use deadlock::db::user_settings;
use deadlock_protocol::auth::{
    DeviceCode,
    DevicePoll,
    DevicePollRequest,
    DeviceSecret,
    Me,
    Tier,
};
use deadlock_protocol::error::{ApiError, ErrorCode};
use http_body_util::BodyExt;
use secrecy::SecretString;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
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
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let config = config();
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let state = WebState::new(app, &config)?;
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
