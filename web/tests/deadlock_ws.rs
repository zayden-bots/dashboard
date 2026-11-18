use std::error::Error;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use accounts::links::AccountLink;
use accounts::provider::Provider;
use deadlock_protocol::error::{ApiError, ErrorCode};
use deadlock_protocol::ws::{
    CLOSE_ACCOUNT_MISMATCH,
    CLOSE_POLICY,
    CLOSE_UNAUTHORIZED,
    Down,
    MAX_FRAME_BYTES,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use secrecy::SecretString;
use sqlx::PgPool;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use topcoat::asset::{AssetBundle, RouterBuilderAssetExt};
use topcoat::router::request::Request;
use topcoat::router::response::Response;
use topcoat::router::{Body, Method, Router, StatusCode, header};
use web::components::brand::LOGO;
use web::deadlock_app::auth::{hash_secret, new_secret};
use web::deadlock_app::tokens;
use web::deadlock_app::ws::{UPGRADE_LIMIT_BYTES, WsHub, WsTiming, spawn_pusher};
use web::document::{PENDING_SUBMIT, STYLESHEET};
use web::state::WebState;
use zayden_app::config::BotConfig;
use zayden_app::events::listener::EventListener;
use zayden_app::events::{AppEvent, DeadlockAppEvent};
use zayden_app::state::AppState as ZaydenAppState;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

const USER_ID: i64 = 41;
const OTHER_ID: i64 = 43;
const STEAM_ID: &str = "76561197960287930";
const OTHER_STEAM: &str = "76561197960287931";
const ACCOUNT: u32 = 22_202;
const OTHER_ACCOUNT: u32 = 22_203;
const PROTOCOL: (&str, &str) = ("x-deadlock-protocol", "1");
const WS_PATH: &str = "/api/deadlock/v1/ws";
const READ_LIMIT: Duration = Duration::from_secs(5);
const UPGRADE: [(&str, &str); 4] = [
    ("connection", "upgrade"),
    ("upgrade", "websocket"),
    ("sec-websocket-version", "13"),
    ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
];

fn bundle_dir() -> TestResult<PathBuf> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| write_bundle().map_err(|e| e.to_string()))
        .clone()
        .map_err(Into::into)
}

fn write_bundle() -> TestResult<PathBuf> {
    let dir = std::env::temp_dir()
        .join(format!("web-deadlock-ws-assets-{}", std::process::id()));
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

struct Server {
    addr: SocketAddr,
    events: broadcast::Sender<AppEvent>,
    _stop: oneshot::Sender<()>,
}

fn build(
    pool: &PgPool,
    timing: WsTiming,
) -> TestResult<(Router, broadcast::Sender<AppEvent>)> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = config();
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let mut state = WebState::new(Arc::clone(&app), &config)?;
    let hub = Arc::new(WsHub::new(timing));
    state.app_ws = Arc::clone(&hub);
    spawn_pusher(hub, pool.clone(), app.events.subscribe());
    let router = web::router()
        .assets(AssetBundle::load_dir(bundle_dir()?)?)
        .with(state)
        .build();
    Ok((router, app.events.clone()))
}

async fn serve(pool: &PgPool, timing: WsTiming) -> TestResult<Server> {
    let (router, events) = build(pool, timing)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (stop, rx) = oneshot::channel();
    tokio::spawn(topcoat::serve_until(listener, router, async {
        let _ = rx.await;
    }));
    Ok(Server { addr, events, _stop: stop })
}

fn router(pool: &PgPool) -> TestResult<Router> {
    Ok(build(pool, WsTiming::default())?.0)
}

async fn seed_token(pool: &PgPool, discord_id: i64) -> TestResult<(String, i64)> {
    let secret = new_secret();
    let id = tokens::insert(
        &mut *pool.acquire().await?,
        &hash_secret(&secret),
        discord_id,
        "pc",
        "0.1.0",
    )
    .await?;
    Ok((secret, id))
}

async fn link(pool: &PgPool, discord_id: i64, steam_id: &str) -> TestResult {
    AccountLink::upsert_transfer(pool, discord_id, Provider::Steam, steam_id)
        .await?;
    Ok(())
}

async fn refused(
    router: &Router,
    method: Method,
    headers: &[(&str, &str)],
) -> TestResult<(StatusCode, ApiError, Response)> {
    let mut request = Request::builder().method(method).uri(WS_PATH);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router.handle(request.body(Body::empty())?).await;
    let status = response.status();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await?.to_bytes();
    let error = serde_json::from_slice(&bytes).map_err(|e| {
        format!("{status}: {e}: {}", String::from_utf8_lossy(&bytes))
    })?;
    Ok((status, error, Response::from_parts(parts, Body::empty())))
}

fn upgrade_headers<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    UPGRADE.iter().copied().chain(extra.iter().copied()).collect()
}

fn auth_header(token: &str) -> String {
    format!("Bearer {token}")
}

async fn connect_raw(addr: SocketAddr, token: &str) -> Result<Client, WsError> {
    let mut request = format!("ws://{addr}{WS_PATH}").into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("authorization", auth_header(token).parse()?);
    headers.insert(PROTOCOL.0, PROTOCOL.1.parse()?);
    Ok(connect_async(request).await?.0)
}

async fn connect(server: &Server, token: &str) -> TestResult<Client> {
    Ok(connect_raw(server.addr, token).await?)
}

async fn send_text(client: &mut Client, text: impl Into<String>) -> TestResult {
    client.send(Message::text(text.into())).await?;
    Ok(())
}

async fn send_json(client: &mut Client, value: &serde_json::Value) -> TestResult {
    send_text(client, value.to_string()).await
}

fn game(event: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "game", "at_ms": 1, "match_time_ms": null, "event": event
    })
}

async fn next_down_raw(client: &mut Client) -> TestResult<Down> {
    loop {
        match client.next().await.ok_or("stream ended")?? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Ping(_) | Message::Pong(_) => {},
            other @ (Message::Binary(_) | Message::Close(_) | Message::Frame(_)) => {
                return Err(format!("unexpected message {other:?}").into());
            },
        }
    }
}

async fn close_code_raw(client: &mut Client) -> TestResult<u16> {
    loop {
        match client.next().await {
            Some(Ok(Message::Close(Some(frame)))) => {
                return Ok(u16::from(frame.code));
            },
            Some(Ok(_)) => {},
            Some(Err(e)) => return Err(e.into()),
            None => return Err("closed without a code".into()),
        }
    }
}

async fn next_down(client: &mut Client) -> TestResult<Down> {
    tokio::time::timeout(READ_LIMIT, next_down_raw(client)).await?
}

async fn close_code(client: &mut Client) -> TestResult<u16> {
    tokio::time::timeout(READ_LIMIT, close_code_raw(client)).await?
}

async fn hello(client: &mut Client) -> TestResult<Down> {
    send_json(
        client,
        &serde_json::json!({
            "type": "hello", "app_version": "0.1.0", "client_version": null
        }),
    )
    .await?;
    next_down(client).await
}

async fn open(pool: &PgPool, timing: WsTiming) -> TestResult<(Server, Client, i64)> {
    let (secret, token_id) = seed_token(pool, USER_ID).await?;
    link(pool, USER_ID, STEAM_ID).await?;
    let server = serve(pool, timing).await?;
    let mut client = connect(&server, &secret).await?;
    assert_eq!(hello(&mut client).await?, Down::Welcome {
        account_id: Some(ACCOUNT)
    });
    Ok((server, client, token_id))
}

async fn open_other(pool: &PgPool, server: &Server) -> TestResult<Client> {
    let (secret, _) = seed_token(pool, OTHER_ID).await?;
    link(pool, OTHER_ID, OTHER_STEAM).await?;
    let mut client = connect(server, &secret).await?;
    assert_eq!(hello(&mut client).await?, Down::Welcome {
        account_id: Some(OTHER_ACCOUNT)
    });
    Ok(client)
}

fn push(server: &Server, event: DeadlockAppEvent) -> TestResult {
    server.events.send(AppEvent::DeadlockApp(event))?;
    Ok(())
}

fn patch(server: &Server, client_version: u32) -> TestResult {
    push(server, DeadlockAppEvent::Patch { client_version })
}

async fn expect_patch(client: &mut Client, client_version: u32) -> TestResult {
    assert_eq!(next_down(client).await?, Down::Patch { client_version });
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_origin_header_is_refused_first(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let headers =
        upgrade_headers(&[("host", "localhost"), ("origin", "http://localhost")]);
    let (status, error, _) = refused(&router, Method::GET, &headers).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error.code, ErrorCode::BadRequest);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_missing_protocol_header_is_426(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let (status, error, _) =
        refused(&router, Method::GET, &upgrade_headers(&[])).await?;
    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    assert_eq!(error.code, ErrorCode::ProtocolUnsupported);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_bad_token_is_401(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let bearer = auth_header(&new_secret());
    let headers = upgrade_headers(&[PROTOCOL, ("authorization", &bearer)]);
    let (status, error, _) = refused(&router, Method::GET, &headers).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error.code, ErrorCode::Unauthorized);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_unlinked_user_is_403_not_linked(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    let bearer = auth_header(&secret);
    let headers = upgrade_headers(&[PROTOCOL, ("authorization", &bearer)]);
    let (status, error, _) = refused(&router, Method::GET, &headers).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error.code, ErrorCode::NotLinked);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_seventh_upgrade_in_a_minute_is_429(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    link(&pool, USER_ID, STEAM_ID).await?;
    let bearer = auth_header(&secret);
    let headers = upgrade_headers(&[PROTOCOL, ("authorization", &bearer)]);
    for _ in 0..6 {
        let (status, error, _) = refused(&router, Method::GET, &headers).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, ErrorCode::BadRequest);
    }
    let (status, error, response) = refused(&router, Method::GET, &headers).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error.code, ErrorCode::RateLimited);
    assert!(error.retry_after_secs.is_some());
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_plain_get_is_400(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    link(&pool, USER_ID, STEAM_ID).await?;
    let bearer = auth_header(&secret);
    let headers = [PROTOCOL, ("authorization", &bearer)];
    let (status, error, _) = refused(&router, Method::GET, &headers).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error.code, ErrorCode::BadRequest);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_post_is_400(pool: PgPool) -> TestResult {
    let router = router(&pool)?;
    let (status, error, _) = refused(&router, Method::POST, &[PROTOCOL]).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error.code, ErrorCode::BadRequest);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn hello_gets_welcome_with_the_linked_account(pool: PgPool) -> TestResult {
    open(&pool, WsTiming::default()).await?;
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_real_refusal_never_upgrades(pool: PgPool) -> TestResult {
    let (server, _client, _) = open(&pool, WsTiming::default()).await?;
    match connect_raw(server.addr, &new_secret()).await {
        Err(WsError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        },
        Err(other) => return Err(other.into()),
        Ok(_) => return Err("a bad token upgraded".into()),
    }
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_first_frame_that_is_not_hello_closes_1008(pool: PgPool) -> TestResult {
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    link(&pool, USER_ID, STEAM_ID).await?;
    let server = serve(&pool, WsTiming::default()).await?;
    let mut client = connect(&server, &secret).await?;
    send_json(
        &mut client,
        &game(&serde_json::json!({ "kind": "pause", "paused": true })),
    )
    .await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_long_app_version_closes_1008(pool: PgPool) -> TestResult {
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    link(&pool, USER_ID, STEAM_ID).await?;
    let server = serve(&pool, WsTiming::default()).await?;
    let mut client = connect(&server, &secret).await?;
    send_json(
        &mut client,
        &serde_json::json!({
            "type": "hello", "app_version": "v".repeat(65), "client_version": null
        }),
    )
    .await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_binary_frame_closes_1008(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    client.send(Message::Binary(vec![1, 2, 3].into())).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_oversized_frame_closes_1008(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    send_text(&mut client, " ".repeat(MAX_FRAME_BYTES + 1)).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_long_string_closes_1008(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let event = serde_json::json!({ "kind": "hero", "name": "h".repeat(65) });
    send_json(&mut client, &game(&event)).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_long_roster_closes_1008(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let event =
        serde_json::json!({ "kind": "roster", "enemies": [1, 2, 3, 4, 5, 6, 7] });
    send_json(&mut client, &game(&event)).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn twenty_one_frames_in_a_second_close_1008(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let frame = game(&serde_json::json!({ "kind": "pause", "paused": true }));
    for _ in 0..21 {
        send_json(&mut client, &frame).await?;
    }
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_overlong_frame_ends_the_socket(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    send_text(&mut client, " ".repeat(UPGRADE_LIMIT_BYTES + 1)).await?;
    tokio::time::timeout(READ_LIMIT, async {
        loop {
            match client.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {},
            }
        }
    })
    .await?;
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_bad_frame_gets_an_error_and_the_socket_stays_open(
    pool: PgPool,
) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    send_text(&mut client, r#"{"type":"game"}"#).await?;
    let Down::Error { error } = next_down(&mut client).await? else {
        return Err("expected an error frame".into());
    };
    assert_eq!(error.code, ErrorCode::BadRequest);
    patch(&server, 1)?;
    expect_patch(&mut client, 1).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_unknown_frame_type_is_ignored(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    send_text(&mut client, r#"{"type":"later"}"#).await?;
    patch(&server, 2)?;
    expect_patch(&mut client, 2).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn game_events_are_accepted_and_dropped(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let events = [
        serde_json::json!({ "kind": "phase", "phase": "in_match" }),
        serde_json::json!({ "kind": "hero", "name": "Haze" }),
        serde_json::json!({ "kind": "roster", "enemies": [1, 2, 3, 4, 5, 6] }),
        serde_json::json!({ "kind": "account", "id": ACCOUNT }),
    ];
    for event in &events {
        send_json(&mut client, &game(event)).await?;
    }
    patch(&server, 3)?;
    expect_patch(&mut client, 3).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_server_pings_on_its_interval(pool: PgPool) -> TestResult {
    let timing =
        WsTiming { ping: Duration::from_millis(200), ..WsTiming::default() };
    let (_server, mut client, _) = open(&pool, timing).await?;
    let start = Instant::now();
    let message = tokio::time::timeout(READ_LIMIT, client.next())
        .await?
        .ok_or("stream ended")??;
    assert!(matches!(message, Message::Ping(_)));
    assert!(start.elapsed() >= Duration::from_millis(100));
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_silent_client_is_dropped(pool: PgPool) -> TestResult {
    let timing =
        WsTiming { idle: Duration::from_millis(300), ..WsTiming::default() };
    let (_server, mut client, _) = open(&pool, timing).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_hello_must_come_in_time(pool: PgPool) -> TestResult {
    let timing =
        WsTiming { hello: Duration::from_millis(300), ..WsTiming::default() };
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    link(&pool, USER_ID, STEAM_ID).await?;
    let server = serve(&pool, timing).await?;
    let mut client = connect(&server, &secret).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_POLICY);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_revoked_token_closes_4401(pool: PgPool) -> TestResult {
    let (server, mut client, token_id) = open(&pool, WsTiming::default()).await?;
    let mut other = open_other(&pool, &server).await?;
    push(&server, DeadlockAppEvent::TokenRevoked { token_id })?;
    assert_eq!(close_code(&mut client).await?, CLOSE_UNAUTHORIZED);
    patch(&server, 4)?;
    expect_patch(&mut other, 4).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_resync_closes_sockets_of_revoked_tokens(pool: PgPool) -> TestResult {
    let (server, mut client, token_id) = open(&pool, WsTiming::default()).await?;
    assert!(tokens::revoke(&pool, token_id).await?);
    server.events.send(AppEvent::Resync)?;
    assert_eq!(close_code(&mut client).await?, CLOSE_UNAUTHORIZED);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn revoke_reaches_the_socket_through_postgres(pool: PgPool) -> TestResult {
    let (server, mut client, token_id) = open(&pool, WsTiming::default()).await?;
    EventListener::spawn(pool.clone(), server.events.clone());
    let mut listening = false;
    for _ in 0..50 {
        DeadlockAppEvent::Patch { client_version: 1 }.notify(&pool).await?;
        if tokio::time::timeout(
            Duration::from_millis(200),
            next_down_raw(&mut client),
        )
        .await
        .is_ok()
        {
            listening = true;
            break;
        }
    }
    assert!(listening, "the listener never started");
    assert!(tokens::revoke(&pool, token_id).await?);
    assert_eq!(close_code(&mut client).await?, CLOSE_UNAUTHORIZED);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn another_account_in_game_closes_4403(pool: PgPool) -> TestResult {
    let (_server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let event = serde_json::json!({ "kind": "account", "id": OTHER_ACCOUNT });
    send_json(&mut client, &game(&event)).await?;
    assert_eq!(close_code(&mut client).await?, CLOSE_ACCOUNT_MISMATCH);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn relinking_closes_4403(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    link(&pool, USER_ID, OTHER_STEAM).await?;
    push(&server, DeadlockAppEvent::LinkChanged { discord_id: USER_ID })?;
    assert_eq!(close_code(&mut client).await?, CLOSE_ACCOUNT_MISMATCH);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn unlinking_closes_4403(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    assert!(AccountLink::delete(&pool, USER_ID, Provider::Steam).await?);
    push(&server, DeadlockAppEvent::LinkChanged { discord_id: USER_ID })?;
    assert_eq!(close_code(&mut client).await?, CLOSE_ACCOUNT_MISMATCH);
    Ok(())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_unchanged_link_keeps_the_socket(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    push(&server, DeadlockAppEvent::LinkChanged { discord_id: USER_ID })?;
    patch(&server, 5)?;
    expect_patch(&mut client, 5).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn patch_reaches_every_socket(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let mut other = open_other(&pool, &server).await?;
    patch(&server, 6_200)?;
    expect_patch(&mut client, 6_200).await?;
    expect_patch(&mut other, 6_200).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn match_ready_reaches_only_its_user(pool: PgPool) -> TestResult {
    let (server, mut client, _) = open(&pool, WsTiming::default()).await?;
    let mut other = open_other(&pool, &server).await?;
    push(&server, DeadlockAppEvent::MatchReady {
        discord_id: USER_ID,
        match_id: 9,
    })?;
    patch(&server, 8)?;
    assert_eq!(next_down(&mut client).await?, Down::MatchReady { match_id: 9 });
    expect_patch(&mut other, 8).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_fourth_socket_closes_the_oldest(pool: PgPool) -> TestResult {
    let (secret, _) = seed_token(&pool, USER_ID).await?;
    link(&pool, USER_ID, STEAM_ID).await?;
    let server = serve(&pool, WsTiming::default()).await?;
    let mut first = connect(&server, &secret).await?;
    assert!(matches!(hello(&mut first).await?, Down::Welcome { .. }));
    let mut others = Vec::new();
    for _ in 0..3 {
        let mut client = connect(&server, &secret).await?;
        assert!(matches!(hello(&mut client).await?, Down::Welcome { .. }));
        others.push(client);
    }
    assert_eq!(close_code(&mut first).await?, CLOSE_POLICY);
    assert_eq!(others.len(), 3);
    Ok(())
}
