//! Per-application REST clients and the guild client choice, against Postgres.

use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use secrecy::SecretString;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use web::auth::{GuildAccess, guild_admin_for};
use web::rest::{ClientBuilder, RestClients};
use web::state::{DiscordState, SessionIdentity, WebState};
use zayden_app::applications::{ApplicationStore, Keyring, NewApplication, Slot};
use zayden_app::config::BotConfig;
use zayden_app::guilds::GuildPresence;
use zayden_app::state::AppState as ZaydenAppState;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const MARIA_TOKEN: &str = "maria-token";
const TEST_POOL_CONNECTIONS: u32 = 2;

type Recorder = Arc<Mutex<Vec<String>>>;

struct Harness {
    state: WebState,
    recorder: Recorder,
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
        maria_id: Some(3),
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

async fn test_pool(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) -> TestResult<PgPool> {
    Ok(options.max_connections(TEST_POOL_CONNECTIONS).connect_with(connect).await?)
}

fn unauthorized_discord() -> TestResult<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?.to_string();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let _ = reject(stream);
            });
        }
    });
    Ok(addr)
}

fn reject(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let body = r#"{"message":"401: Unauthorized","code":0}"#;
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

fn client(addr: &str, token: String) -> twilight_http::Client {
    twilight_http::Client::builder()
        .token(token)
        .proxy(addr.to_owned(), true)
        .ratelimiter(None)
        .build()
}

fn recording_builder(addr: String, recorder: &Recorder) -> ClientBuilder {
    let recorder = Arc::clone(recorder);
    Arc::new(move |token: String| {
        if let Ok(mut tokens) = recorder.lock() {
            tokens.push(token.clone());
        }
        client(&addr, token)
    })
}

fn harness(pool: &PgPool, keyring: bool) -> TestResult<Harness> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let config = config();
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let mut state = WebState::new(Arc::clone(&app), &config)?;
    let addr = unauthorized_discord()?;
    state.discord = DiscordState {
        http: Arc::new(client(&addr, "test-bot-token".to_owned())),
        user_guilds: state.discord.user_guilds,
        users: state.discord.users,
        member_names: state.discord.member_names,
    };

    let recorder = Recorder::default();
    let keyring = if keyring {
        Some(Arc::new(Keyring::parse(&format!("1:{KEY}"), "1")?))
    } else {
        None
    };
    state.rest = RestClients::new(keyring, recording_builder(addr, &recorder));

    Ok(Harness { state, recorder })
}

async fn register(
    pool: &PgPool,
    application_id: i64,
    slot: Slot,
    name: &str,
) -> TestResult {
    ApplicationStore::register_default(pool, &NewApplication {
        application_id,
        slot,
        bot_user_id: application_id * 10,
        name,
        avatar: None,
    })
    .await?;
    Ok(())
}

async fn join(pool: &PgPool, application_id: i64) -> TestResult {
    GuildPresence::joined(pool, 7, application_id, jiff::Timestamp::now()).await?;
    Ok(())
}

async fn publish_token(pool: &PgPool, token: &str) -> TestResult {
    let keyring = Keyring::parse(&format!("1:{KEY}"), "1")?;
    ApplicationStore::publish_default_token(
        pool,
        &keyring,
        3,
        &SecretString::from(token),
    )
    .await?;
    Ok(())
}

async fn publish_maria_token(pool: &PgPool) -> TestResult {
    publish_token(pool, MARIA_TOKEN).await
}

async fn unseated_custom(pool: &PgPool, token: &str) -> TestResult {
    let keyring = Keyring::parse(&format!("1:{KEY}"), "1")?;
    let sealed = keyring.seal(2000, &SecretString::from(token))?;
    sqlx::query!(
        "INSERT INTO applications (application_id, slot, kind, bot_user_id, name, public_key, token_ct, token_nonce, key_id, registered_by)
         VALUES (2000, 'zayden', 'custom', 2001, 'Custom', 'key', $1, $2, $3, 1)",
        sealed.ciphertext,
        sealed.nonce,
        sealed.key_id
    )
    .execute(pool)
    .await?;
    join(pool, 2000).await
}

async fn both_bots(pool: &PgPool) -> TestResult {
    register(pool, 1, Slot::Zayden, "Zayden").await?;
    register(pool, 3, Slot::Maria, "Maria").await
}

fn recorded(recorder: &Recorder) -> Vec<String> {
    recorder.lock().map_or_else(|_| Vec::new(), |tokens| tokens.clone())
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_zayden_application_uses_the_dashboard_client(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    let app = harness(&pool, true).unwrap();

    let client = app.state.app_client(1).await.unwrap();

    assert!(Arc::ptr_eq(&client, &app.state.discord.http));
    assert_eq!(recorded(&app.recorder), Vec::<String>::new());

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn without_a_keyring_another_application_has_no_client(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    publish_maria_token(&pool).await.unwrap();
    let app = harness(&pool, false).unwrap();

    assert!(app.state.app_client(3).await.is_none());
    assert_eq!(recorded(&app.recorder), Vec::<String>::new());

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_sealed_token_opens_once_and_is_cached(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    publish_maria_token(&pool).await.unwrap();
    let app = harness(&pool, true).unwrap();

    let first = app.state.app_client(3).await;
    let second = app.state.app_client(3).await;

    assert!(first.is_some());
    assert!(second.is_some());
    assert_eq!(recorded(&app.recorder), vec![MARIA_TOKEN.to_owned()]);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_changed_token_opens_a_new_client(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    publish_token(&pool, "token-a").await.unwrap();
    let app = harness(&pool, true).unwrap();

    assert!(app.state.app_client(3).await.is_some());
    publish_token(&pool, "token-b").await.unwrap();
    assert!(app.state.app_client(3).await.is_some());

    assert_eq!(recorded(&app.recorder), vec![
        "token-a".to_owned(),
        "token-b".to_owned()
    ]);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn guild_client_skips_an_unseated_custom_bot(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    publish_maria_token(&pool).await.unwrap();
    unseated_custom(&pool, "custom-token").await.unwrap();
    join(&pool, 3).await.unwrap();
    let app = harness(&pool, true).unwrap();

    let _client = app.state.guild_client(7).await;

    assert_eq!(recorded(&app.recorder), vec![MARIA_TOKEN.to_owned()]);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_application_without_a_token_has_no_client(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    let app = harness(&pool, true).unwrap();

    assert!(app.state.app_client(3).await.is_none());
    assert_eq!(recorded(&app.recorder), Vec::<String>::new());

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn guild_client_prefers_the_present_zayden_seat(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    publish_maria_token(&pool).await.unwrap();
    join(&pool, 1).await.unwrap();
    join(&pool, 3).await.unwrap();
    let app = harness(&pool, true).unwrap();

    let client = app.state.guild_client(7).await;

    assert!(Arc::ptr_eq(&client, &app.state.discord.http));
    assert_eq!(recorded(&app.recorder), Vec::<String>::new());

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn guild_client_uses_another_present_bot_when_the_seat_is_absent(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    publish_maria_token(&pool).await.unwrap();
    join(&pool, 3).await.unwrap();
    let app = harness(&pool, true).unwrap();

    let client = app.state.guild_client(7).await;

    assert!(!Arc::ptr_eq(&client, &app.state.discord.http));
    assert_eq!(recorded(&app.recorder), vec![MARIA_TOKEN.to_owned()]);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn guild_client_falls_back_to_the_dashboard_client(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    let app = harness(&pool, true).unwrap();

    let client = app.state.guild_client(7).await;

    assert!(Arc::ptr_eq(&client, &app.state.discord.http));

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_operator_passes_the_gate_when_a_bot_is_present(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    join(&pool, 3).await.unwrap();
    sqlx::query!(
        "INSERT INTO web_user_roles (discord_user_id, role) VALUES ($1, $2)",
        42_i64,
        "operator"
    )
    .execute(&pool)
    .await
    .unwrap();
    let app = harness(&pool, true).unwrap();
    app.state.discord.user_guilds.insert(42, Arc::from([])).await;
    let identity = SessionIdentity {
        user_id: 42,
        access_token: "staff-token-access".to_owned(),
    };

    let context = guild_admin_for(
        &pool,
        &identity,
        "7",
        Some(app.state.discord.http.as_ref()),
        Some(&app.state.discord.user_guilds),
    )
    .await
    .unwrap();

    assert_eq!(context.access, GuildAccess::Operator);

    pool.close().await;
}
