use std::error::Error;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::thread;

use http_body_util::BodyExt;
use jiff::Timestamp;
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
use web::admin::status::{ProcessView, process_view};
use web::auth::SessionUser;
use web::components::brand::LOGO;
use web::document::{PENDING_SUBMIT, STYLESHEET};
use web::state::{DiscordState, SessionIdentity, WebState};
use zayden_app::applications::{
    ApplicationStore,
    NewApplication,
    ProcessRow,
    ProcessStatus,
    Slot,
};
use zayden_app::config::BotConfig;
use zayden_app::state::AppState as ZaydenAppState;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const MEMBER: &str = "session=member-token";
const STAFF: &str = "session=staff-token";
const TEST_POOL_CONNECTIONS: u32 = 2;
const NOW: i64 = 1_800_000_000;

fn bundle_dir() -> TestResult<PathBuf> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| write_bundle().map_err(|e| e.to_string()))
        .clone()
        .map_err(Into::into)
}

fn write_bundle() -> TestResult<PathBuf> {
    let dir = std::env::temp_dir()
        .join(format!("web-status-assets-{}", std::process::id()));
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

fn guild(id: u64, name: &str) -> CurrentUserGuild {
    CurrentUserGuild {
        id: Id::new(id),
        name: name.to_owned(),
        icon: None,
        owner: false,
        permissions: Permissions::MANAGE_GUILD,
        features: Vec::new(),
    }
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

async fn harness(pool: &PgPool) -> TestResult<Router> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let config = config();
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let mut state = WebState::new(app, &config)?;
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

    for (token, user_id) in [("member-token", 41), ("staff-token", 42)] {
        state
            .sessions
            .insert(token.to_owned(), SessionIdentity {
                user_id,
                access_token: format!("{token}-access"),
            })
            .await;
    }
    state.discord.user_guilds.insert(41, Arc::from([guild(7, "Guild 7")])).await;
    state.discord.user_guilds.insert(42, Arc::from([])).await;
    for id in [41_i64, 42] {
        state
            .discord
            .users
            .insert(id, SessionUser {
                id: id.to_string(),
                name: format!("User {id}"),
                avatar: None,
            })
            .await;
    }

    Ok(web::router()
        .assets(AssetBundle::load_dir(bundle_dir()?)?)
        .app_context(state)
        .build())
}

async fn get(router: &Router, path: &str, cookie: &str) -> TestResult<Response> {
    let request = Request::get(path).header(header::COOKIE, cookie);
    Ok(router.handle(request.body(Body::empty())?).await)
}

async fn grant_role(pool: &PgPool, user_id: i64, role: &str) -> TestResult {
    sqlx::query!(
        "INSERT INTO web_user_roles (discord_user_id, role) VALUES ($1, $2)",
        user_id,
        role
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[expect(
    clippy::unwrap_used,
    reason = "a free helper sits outside the #[test] items clippy.toml exempts"
)]
fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn row(process: &str, name: Option<&str>, last_seen: i64) -> ProcessRow {
    ProcessRow {
        process: process.to_owned(),
        application_id: None,
        name: name.map(str::to_owned),
        build_sha: "0123456789abcdef".to_owned(),
        started_at: at(NOW - 3600),
        last_seen: at(last_seen),
    }
}

fn view(process: &str, name: Option<&str>, last_seen: i64) -> ProcessView {
    process_view(&row(process, name, last_seen), at(NOW))
}

#[test]
fn a_recent_heartbeat_is_live() {
    assert!(view("100", Some("Zayden"), NOW - 150).live);
}

#[test]
fn a_heartbeat_older_than_150s_is_stale() {
    assert!(!view("100", Some("Zayden"), NOW - 151).live);
}

#[test]
fn the_worker_row_is_labelled_worker() {
    assert_eq!(view("worker", None, NOW).label, "Worker");
    assert_eq!(view("100", Some("Zayden"), NOW).label, "Zayden");
    assert_eq!(view("100", None, NOW).label, "100");
}

#[test]
fn the_build_sha_is_shortened() {
    let shown = view("worker", None, NOW);

    assert_eq!(shown.build, "0123456");
    assert_eq!(shown.started, "2027-01-15 07:00 UTC");
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn operators_see_the_process_list_and_members_are_refused(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = options
        .max_connections(TEST_POOL_CONNECTIONS)
        .connect_with(connect)
        .await
        .unwrap();
    grant_role(&pool, 42, "operator").await.unwrap();
    let application = NewApplication {
        application_id: 100,
        slot: Slot::Zayden,
        bot_user_id: 100,
        name: "Statusbot",
        avatar: None,
    };
    ApplicationStore::register_default(&pool, &application).await.unwrap();
    ProcessStatus::started(&pool, "100", Some(100), "abcdef0123").await.unwrap();
    let router = harness(&pool).await.unwrap();

    let response = get(&router, "/admin/status", STAFF).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("Statusbot"), "{html}");
    assert!(html.contains("abcdef0"), "{html}");

    let response = get(&router, "/admin/status", MEMBER).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    pool.close().await;
}
