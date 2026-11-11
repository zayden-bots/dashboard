//! The guild overview's bot tabs, per-bot rack, theme and duplicate warning
//! through the app router, against Postgres.

use std::error::Error;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::thread;

use http_body_util::BodyExt;
use secrecy::SecretString;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use topcoat::asset::{AssetBundle, RouterBuilderAssetExt};
use topcoat::router::request::Request;
use topcoat::router::response::Response;
use topcoat::router::{Body, Router, StatusCode, header};
use twilight_model::guild::Permissions;
use twilight_model::id::Id;
use twilight_model::user::CurrentUserGuild;
use web::auth::SessionUser;
use web::components::brand::LOGO;
use web::document::{PENDING_SUBMIT, STYLESHEET};
use web::state::{DiscordState, SessionIdentity, SessionUsersCache, WebState};
use zayden_app::applications::{
    ApplicationModules,
    ApplicationStore,
    NewApplication,
    Slot,
};
use zayden_app::config::BotConfig;
use zayden_app::guilds::GuildPresence;
use zayden_app::state::AppState as ZaydenAppState;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const MEMBER: &str = "session=member-token";
const FORM: &str = "application/x-www-form-urlencoded";
const TEST_POOL_CONNECTIONS: u32 = 2;
const TAB_MARK: &str = "aria-[current=page]:bg-foreground/10";

fn bundle_dir() -> TestResult<PathBuf> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| write_bundle().map_err(|e| e.to_string()))
        .clone()
        .map_err(Into::into)
}

fn write_bundle() -> TestResult<PathBuf> {
    let dir = std::env::temp_dir()
        .join(format!("web-seats-assets-{}", std::process::id()));
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

fn config(maria_id: Option<u64>) -> BotConfig {
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
        maria_id,
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

fn guild(id: u64, name: &str, permissions: Permissions) -> CurrentUserGuild {
    CurrentUserGuild {
        id: Id::new(id),
        name: name.to_owned(),
        icon: None,
        owner: false,
        permissions,
        features: Vec::new(),
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

struct Harness {
    router: Router,
    app: Arc<ZaydenAppState>,
}

async fn seed_users(users: &SessionUsersCache, ids: &[i64]) {
    for &id in ids {
        users
            .insert(id, SessionUser {
                id: id.to_string(),
                name: format!("User {id}"),
                avatar: None,
            })
            .await;
    }
}

async fn harness(pool: &PgPool, maria_id: Option<u64>) -> TestResult<Harness> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let config = config(maria_id);
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let mut state = WebState::new(Arc::clone(&app), &config)?;
    state.discord = DiscordState {
        http: Arc::new(
            twilight_http::Client::builder()
                .token("test-bot-token".to_owned())
                .proxy(unauthorized_discord()?, true)
                .ratelimiter(None)
                .build(),
        ),
        user_guilds: state.discord.user_guilds,
        users: state.discord.users,
        member_names: state.discord.member_names,
    };
    state
        .sessions
        .insert("member-token".to_owned(), SessionIdentity {
            user_id: 41,
            access_token: "member-token-access".to_owned(),
        })
        .await;
    state
        .discord
        .user_guilds
        .insert(41, Arc::from([guild(7, "Guild 7", Permissions::MANAGE_GUILD)]))
        .await;
    seed_users(&state.discord.users, &[41]).await;

    let router = web::router()
        .assets(AssetBundle::load_dir(bundle_dir()?)?)
        .app_context(state)
        .build();
    Ok(Harness { router, app })
}

impl Harness {
    async fn get(&self, path: &str) -> TestResult<Response> {
        let request = Request::get(path).header(header::COOKIE, MEMBER);
        Ok(self.router.handle(request.body(Body::empty())?).await)
    }

    async fn post(&self, body: &str) -> TestResult<Response> {
        let request = Request::post("/guild/7")
            .header(header::CONTENT_TYPE, FORM)
            .header(header::COOKIE, MEMBER);
        Ok(self.router.handle(request.body(Body::from(body.to_owned()))?).await)
    }

    async fn page(&self, path: &str) -> TestResult<String> {
        let response = self.get(path).await?;
        if response.status() != StatusCode::OK {
            return Err(format!("{path} answered {}", response.status()).into());
        }
        body_text(response).await
    }
}

async fn body_text(response: Response) -> TestResult<String> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(strip_comments(&String::from_utf8(bytes.to_vec())?))
}

fn strip_comments(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some((before, after)) = rest.split_once("<!--") {
        out.push_str(before);
        rest = after.split_once("-->").map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out
}

fn location(response: &Response) -> Option<&str> {
    response.headers().get(header::LOCATION).and_then(|v| v.to_str().ok())
}

fn anchor_with(html: &str, href: &str) -> Option<String> {
    let needle = format!(r#"href="{href}""#);
    let (before, after) = html.split_once(&needle)?;
    let (_, opening) = before.rsplit_once("<a ")?;
    let (rest, _) = after.split_once('>')?;
    Some(format!("{opening}{needle}{rest}"))
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

async fn offer(pool: &PgPool, application_id: i64, ids: &[&str]) -> TestResult {
    let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    ApplicationModules::replace(pool, application_id, &ids).await?;
    Ok(())
}

async fn both_bots(pool: &PgPool) -> TestResult {
    register(pool, 1, Slot::Zayden, "Zayden").await?;
    register(pool, 3, Slot::Maria, "Maria").await?;
    join(pool, 1).await?;
    join(pool, 3).await
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn without_maria_configured_the_overview_has_one_tab(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    register(&pool, 1, Slot::Zayden, "Zayden").await.unwrap();
    join(&pool, 1).await.unwrap();
    let app = harness(&pool, None).await.unwrap();

    let html = app.page("/guild/7").await.unwrap();

    assert_eq!(html.matches(TAB_MARK).count(), 1, "{html}");
    assert!(html.contains(r#"data-bot="zayden""#), "{html}");
    assert!(!html.contains("bot=maria"), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn bot_maria_selects_the_maria_rack_and_theme(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    offer(&pool, 3, &["gambling"]).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();
    app.app.modules.set(7, 3, "gambling", true).await.unwrap();
    app.app.modules.set(7, 3, "music", true).await.unwrap();

    let html = app.page("/guild/7?bot=maria").await.unwrap();

    assert!(html.contains(r#"data-bot="maria""#), "{html}");
    assert_eq!(html.matches(TAB_MARK).count(), 2, "{html}");
    let maria = anchor_with(&html, "/guild/7?bot=maria").unwrap();
    assert!(maria.contains(r#"aria-current="page""#), "{maria}");
    assert!(
        html.contains(r#"<input type="hidden" name="bot" value="maria">"#),
        "{html}"
    );
    assert!(html.contains(r#"action="/guild/7?bot=maria""#), "{html}");
    assert!(
        html.contains(r#"aria-label="Gambling &amp; Economy module""#),
        "{html}"
    );
    assert!(!html.contains(r#"aria-label="Music module""#), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_unknown_bot_falls_back_to_zayden(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();

    let html = app.page("/guild/7?bot=enzo").await.unwrap();

    assert!(html.contains(r#"data-bot="zayden""#), "{html}");
    assert!(!html.contains(r#"data-bot="maria""#), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_maria_toggle_writes_maria_rows_and_redirects_back(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    offer(&pool, 3, &["gambling"]).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();
    app.app.modules.set(7, 3, "gambling", true).await.unwrap();

    let response = app
        .post("guild=7&module_id=gambling&enabled=false&bot=maria")
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), Some("/guild/7?bot=maria#modules-community"));
    assert_eq!(
        app.app.modules.states(7, 3).await.unwrap().get("gambling").copied(),
        Some(false)
    );
    assert_eq!(app.app.modules.states(7, 1).await.unwrap().get("gambling"), None);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_module_the_bot_does_not_offer_is_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    offer(&pool, 3, &["gambling"]).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();

    let response =
        app.post("guild=7&module_id=music&enabled=true&bot=maria").await.unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(response).await.unwrap();
    assert!(html.contains("Not changed: Maria doesn't offer Music."), "{html}");
    assert_eq!(app.app.modules.states(7, 3).await.unwrap().get("music"), None);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_duplicate_bot_field_is_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();

    let response = app
        .post("guild=7&module_id=gambling&enabled=true&bot=maria&bot=maria")
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(response).await.unwrap();
    assert!(html.contains("Not changed: invalid value for `bot`"), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR", fixtures("custom_seat"))]
async fn a_custom_bot_takes_the_zayden_tab(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    let app = harness(&pool, None).await.unwrap();

    let html = app.page("/guild/7").await.unwrap();

    assert_eq!(html.matches(TAB_MARK).count(), 1, "{html}");
    assert_eq!(html.matches(">Custom<").count(), 2, "{html}");
    assert!(!html.contains("Not in server"), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_absent_default_bot_shows_its_invite(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    register(&pool, 1, Slot::Zayden, "Zayden").await.unwrap();
    register(&pool, 3, Slot::Maria, "Maria").await.unwrap();
    join(&pool, 1).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();

    let html = app.page("/guild/7?bot=maria").await.unwrap();

    assert!(html.contains("Maria isn't in this server yet"), "{html}");
    assert!(html.contains(r#"href="/invite?guild=7&amp;bot=maria""#), "{html}");
    assert!(html.contains(">Add Maria<"), "{html}");
    assert_eq!(html.matches("Not in server").count(), 1, "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn two_bots_running_levels_get_a_warning(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    offer(&pool, 1, &["levels"]).await.unwrap();
    offer(&pool, 3, &["levels"]).await.unwrap();
    let app = harness(&pool, Some(3)).await.unwrap();

    let html = app.page("/guild/7").await.unwrap();

    assert!(html.contains("Running on more than one bot"), "{html}");
    assert!(html.contains("Levels is on for Zayden and Maria."), "{html}");

    pool.close().await;
}
