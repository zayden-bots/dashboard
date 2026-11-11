//! Delivery binding: the choice parser, the settings forms that write it and
//! the status the pages show, through the app router against Postgres and a
//! stand-in Discord API.

use std::error::Error;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

use deadlock::db::announce::PatchAnnounceRow;
use deadlock::db::{squad_channel, summary_channel};
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
use web::guild::GuildError;
use web::guild::delivery::{DELIVERY_FIELD, DeliveryChoice, take_delivery_choice};
use web::state::{DiscordState, SessionIdentity, SessionUsersCache, WebState};
use youtube::{YoutubeAnnounceRow, YoutubeConnection};
use zayden_app::applications::{
    ApplicationStore,
    NewApplication,
    SealedToken,
    Slot,
};
use zayden_app::config::BotConfig;
use zayden_app::delivery::{DeliveryKind, DeliveryStore, NewHook};
use zayden_app::guilds::GuildPresence;
use zayden_app::state::AppState as ZaydenAppState;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const MEMBER: &str = "session=member-token";
const FORM: &str = "application/x-www-form-urlencoded";
const TEST_POOL_CONNECTIONS: u32 = 2;
const WEBHOOKS_ALLOWED: &str = "536873984";

struct Reply {
    method: &'static str,
    path: String,
    status: u16,
    body: String,
}

#[derive(Clone)]
struct Discord {
    addr: String,
    replies: Arc<Mutex<Vec<Reply>>>,
}

impl Discord {
    fn start() -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let discord = Self {
            addr: listener.local_addr()?.to_string(),
            replies: Arc::default(),
        };

        let served = discord.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let served = served.clone();
                thread::spawn(move || {
                    let _ = served.serve(stream);
                });
            }
        });

        Ok(discord)
    }

    fn serve(&self, stream: TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut stream = stream;

        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        let mut request = request_line.split_whitespace();
        let method = request.next().unwrap_or_default().to_owned();
        let target = request.next().unwrap_or_default().to_owned();

        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().unwrap_or(0);
            }
        }
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body)?;

        let path = target
            .split('?')
            .next()
            .unwrap_or_default()
            .trim_start_matches("/api/v10")
            .to_owned();
        let (status, reply) = self.answer(&method, &path);

        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{reply}",
            reply.len()
        );
        stream.write_all(response.as_bytes())
    }

    fn answer(&self, method: &str, path: &str) -> (u16, String) {
        locked(&self.replies)
            .iter()
            .rev()
            .find(|r| r.method == method && r.path == path)
            .map_or_else(
                || (404, r#"{"message":"Unknown","code":0}"#.to_owned()),
                |r| (r.status, r.body.clone()),
            )
    }

    fn reply(&self, method: &'static str, path: &str, status: u16, body: &str) {
        locked(&self.replies).push(Reply {
            method,
            path: path.to_owned(),
            status,
            body: body.to_owned(),
        });
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn channel_json(id: u64, name: &str, kind: u8, position: u32) -> String {
    format!(
        r#"{{"id":"{id}","type":{kind},"name":"{name}","position":{position},"guild_id":"7"}}"#
    )
}

fn role_json(id: u64, name: &str, position: i64) -> String {
    format!(
        r#"{{"color":0,"colors":{{"primary_color":0}},"hoist":false,"id":"{id}","managed":false,"mentionable":false,"name":"{name}","permissions":"0","position":{position},"flags":0}}"#
    )
}

fn list(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn member_json(id: u64, name: &str) -> String {
    format!(
        r#"{{"communication_disabled_until":null,"deaf":false,"flags":0,"joined_at":null,"mute":false,"nick":null,"roles":[],"user":{{"accent_color":null,"avatar":null,"avatar_decoration":null,"avatar_decoration_data":null,"banner":null,"discriminator":"0","id":"{id}","username":"{name}"}}}}"#
    )
}

fn channel_with_allow(id: u64, name: &str, allow: &str) -> String {
    format!(
        r#"{{"id":"{id}","type":5,"name":"{name}","guild_id":"7","permission_overwrites":[{{"id":"7","type":0,"allow":"{allow}","deny":"0"}}]}}"#
    )
}

fn serve_guilds(discord: &Discord) {
    let channels =
        list(&[channel_json(100, "rules", 0, 0), channel_json(101, "news", 5, 1)]);
    let roles = list(&[role_json(7, "@everyone", 0), role_json(200, "Mods", 2)]);
    discord.reply("GET", "/guilds/7/channels", 200, &channels);
    discord.reply("GET", "/guilds/7/roles", 200, &roles);
    discord.reply(
        "GET",
        "/users/@me",
        200,
        r#"{"accent_color":null,"avatar":null,"banner":null,"bot":true,"discriminator":"0","id":"900","mfa_enabled":false,"username":"Zayden"}"#,
    );
    discord.reply("GET", "/guilds/7/members/900", 200, &member_json(900, "Zayden"));
    discord.reply(
        "GET",
        "/channels/101",
        200,
        &channel_with_allow(101, "news", WEBHOOKS_ALLOWED),
    );
}

fn bundle_dir() -> TestResult<PathBuf> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| write_bundle().map_err(|e| e.to_string()))
        .clone()
        .map_err(Into::into)
}

fn write_bundle() -> TestResult<PathBuf> {
    let dir = std::env::temp_dir()
        .join(format!("web-delivery-assets-{}", std::process::id()));
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

fn guild(id: u64, permissions: Permissions) -> CurrentUserGuild {
    CurrentUserGuild {
        id: Id::new(id),
        name: format!("Guild {id}"),
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

struct Harness {
    router: Router,
    pool: PgPool,
    discord: Discord,
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

async fn harness(pool: &PgPool) -> TestResult<Harness> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let discord = Discord::start()?;
    serve_guilds(&discord);
    let config = config();
    let app = Arc::new(ZaydenAppState::new(pool.clone(), &config));
    let mut state = WebState::new(Arc::clone(&app), &config)?;
    state.discord = DiscordState {
        http: Arc::new(
            twilight_http::Client::builder()
                .token("test-bot-token".to_owned())
                .proxy(discord.addr.clone(), true)
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
    seed_users(&state.discord.users, &[41]).await;
    state
        .discord
        .user_guilds
        .insert(41, Arc::from([guild(7, Permissions::MANAGE_GUILD)]))
        .await;

    let router = web::router()
        .assets(AssetBundle::load_dir(bundle_dir()?)?)
        .app_context(state)
        .build();
    Ok(Harness { router, pool: pool.clone(), discord })
}

impl Harness {
    async fn page(&self, path: &str) -> TestResult<String> {
        let request = Request::get(path).header(header::COOKIE, MEMBER);
        let response = self.router.handle(request.body(Body::empty())?).await;
        if response.status() != StatusCode::OK {
            return Err(format!("{path} answered {}", response.status()).into());
        }
        body_text(response).await
    }

    async fn post(&self, path: &str, body: &str) -> TestResult<Response> {
        let request = Request::post(path)
            .header(header::CONTENT_TYPE, FORM)
            .header(header::COOKIE, MEMBER);
        Ok(self.router.handle(request.body(Body::from(body.to_owned()))?).await)
    }

    async fn saved(&self, path: &str, body: &str) -> TestResult {
        let response = self.post(path, body).await?;
        if response.status() != StatusCode::SEE_OTHER {
            let got = response.status();
            let html = body_text(response).await?;
            return Err(format!("{path} {body} answered {got}: {html}").into());
        }
        Ok(())
    }

    async fn refused(&self, path: &str, body: &str) -> TestResult<String> {
        let response = self.post(path, body).await?;
        if response.status() != StatusCode::UNPROCESSABLE_ENTITY {
            let got = response.status();
            let html = body_text(response).await?;
            return Err(format!("{path} {body} answered {got}: {html}").into());
        }
        body_text(response).await
    }

    async fn bound(&self, kind: DeliveryKind) -> TestResult<Option<i64>> {
        Ok(DeliveryStore::bound_application(&self.pool, kind, 7).await?)
    }
}

async fn body_text(response: Response) -> TestResult<String> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8(bytes.to_vec())?)
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

async fn both_bots(pool: &PgPool) -> TestResult {
    register(pool, 1, Slot::Zayden, "Zayden").await?;
    register(pool, 3, Slot::Maria, "Maria").await?;
    join(pool, 1).await?;
    join(pool, 3).await
}

async fn connect_youtube(pool: &PgPool, channel: Option<i64>) -> TestResult {
    YoutubeConnection::connect(
        pool,
        7,
        &youtube::model::OwnChannel {
            id: "UC123".to_owned(),
            title: "Seed Channel".to_owned(),
            uploads_playlist_id: "UU123".to_owned(),
        },
        41,
        "s",
    )
    .await?;
    if let Some(channel) = channel {
        YoutubeAnnounceRow::upsert(pool, 7, channel, None).await?;
    }
    Ok(())
}

async fn webhook(pool: &PgPool, channel_id: i64, broken: bool) -> TestResult {
    DeliveryStore::upsert(pool, &NewHook {
        channel_id,
        application_id: 1,
        guild_id: 7,
        webhook_id: 9,
        sealed: SealedToken { key_id: 1, nonce: vec![0; 12], ciphertext: vec![1] },
    })
    .await?;
    if broken {
        DeliveryStore::mark_broken(pool, channel_id, 1, 9).await?;
    }
    Ok(())
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

#[test]
fn an_absent_field_keeps_the_binding() {
    let mut form = pairs(&[("guild", "7")]);
    assert_eq!(take_delivery_choice(&mut form), Ok(DeliveryChoice::Keep));
}

#[test]
fn an_empty_field_chooses_the_seat() {
    let mut form = pairs(&[(DELIVERY_FIELD, "")]);
    assert_eq!(take_delivery_choice(&mut form), Ok(DeliveryChoice::Seat));
    assert_eq!(form, pairs(&[]));
}

#[test]
fn a_number_chooses_that_bot() {
    let mut form = pairs(&[(DELIVERY_FIELD, "3")]);
    assert_eq!(take_delivery_choice(&mut form), Ok(DeliveryChoice::Bot(3)));
}

#[test]
fn a_duplicate_field_is_refused() {
    let mut form = pairs(&[(DELIVERY_FIELD, "3"), (DELIVERY_FIELD, "3")]);
    assert_eq!(
        take_delivery_choice(&mut form),
        Err(GuildError::InvalidField(DELIVERY_FIELD))
    );
}

#[test]
fn a_non_number_is_refused() {
    for value in ["maria", "0", "-3", " 3"] {
        let mut form = pairs(&[(DELIVERY_FIELD, value)]);
        assert_eq!(
            take_delivery_choice(&mut form),
            Err(GuildError::InvalidField(DELIVERY_FIELD)),
            "{value}"
        );
    }
}

#[test]
fn other_pairs_keep_their_order() {
    let mut form = pairs(&[
        ("guild", "7"),
        (DELIVERY_FIELD, "3"),
        ("channel_id", "100"),
        ("message", ""),
    ]);
    take_delivery_choice(&mut form).unwrap();
    assert_eq!(
        form,
        pairs(&[("guild", "7"), ("channel_id", "100"), ("message", "")])
    );
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn binding_youtube_to_maria_writes_the_column(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    connect_youtube(&pool, None).await.unwrap();
    let app = harness(&pool).await.unwrap();

    app.saved(
        "/guild/7/youtube",
        "guild=7&channel_id=101&message=&delivery_application_id=3",
    )
    .await
    .unwrap();

    assert_eq!(app.bound(DeliveryKind::Youtube).await.unwrap(), Some(3));

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_seat_choice_clears_the_binding(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    DeliveryStore::bind_application(&pool, DeliveryKind::Youtube, 7, Some(3))
        .await
        .unwrap();
    let app = harness(&pool).await.unwrap();

    app.saved(
        "/guild/7/youtube",
        "guild=7&channel_id=101&message=&delivery_application_id=",
    )
    .await
    .unwrap();

    assert_eq!(app.bound(DeliveryKind::Youtube).await.unwrap(), None);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn a_form_without_the_field_keeps_the_binding(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    DeliveryStore::bind_application(&pool, DeliveryKind::Youtube, 7, Some(3))
        .await
        .unwrap();
    let app = harness(&pool).await.unwrap();

    app.saved("/guild/7/youtube", "guild=7&channel_id=101&message=").await.unwrap();

    assert_eq!(app.bound(DeliveryKind::Youtube).await.unwrap(), Some(3));

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_absent_bot_cannot_be_chosen(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    let app = harness(&pool).await.unwrap();

    let html = app
        .refused(
            "/guild/7/youtube",
            "guild=7&channel_id=101&message=&delivery_application_id=4",
        )
        .await
        .unwrap();

    assert!(html.contains("invalid value for `delivery_application_id`"), "{html}");
    assert_eq!(app.bound(DeliveryKind::Youtube).await.unwrap(), None);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn zayden_without_manage_webhooks_can_still_save(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    connect_youtube(&pool, None).await.unwrap();
    let app = harness(&pool).await.unwrap();
    app.discord.reply(
        "GET",
        "/channels/101",
        200,
        &channel_with_allow(101, "news", "3072"),
    );

    app.saved("/guild/7/youtube", "guild=7&channel_id=101&message=").await.unwrap();

    let rows = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM youtube_announce WHERE guild_id = 7"#
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 1);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn an_unseated_custom_bot_cannot_be_chosen(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    sqlx::query!(
        "INSERT INTO applications (application_id, slot, kind, bot_user_id, name, public_key, token_ct, token_nonce, key_id, registered_by)
         VALUES (2000, 'zayden', 'custom', 2001, 'Custom', 'key', '\\x00'::bytea, '\\x00'::bytea, 1, 1)"
    )
    .execute(&pool)
    .await
    .unwrap();
    join(&pool, 2000).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    let app = harness(&pool).await.unwrap();

    let html = app
        .refused(
            "/guild/7/youtube",
            "guild=7&channel_id=101&message=&delivery_application_id=2000",
        )
        .await
        .unwrap();

    assert!(html.contains("invalid value for `delivery_application_id`"), "{html}");
    assert_eq!(app.bound(DeliveryKind::Youtube).await.unwrap(), None);

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn resaving_with_a_departed_bound_bot_keeps_the_binding(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    register(&pool, 1, Slot::Zayden, "Zayden").await.unwrap();
    register(&pool, 3, Slot::Maria, "Maria").await.unwrap();
    join(&pool, 1).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    DeliveryStore::bind_application(&pool, DeliveryKind::Youtube, 7, Some(3))
        .await
        .unwrap();
    let app = harness(&pool).await.unwrap();

    let html = app.page("/guild/7/youtube").await.unwrap();
    assert!(html.contains("Maria (not in server)"), "{html}");

    app.saved(
        "/guild/7/youtube",
        "guild=7&channel_id=101&message=&delivery_application_id=3",
    )
    .await
    .unwrap();

    assert_eq!(app.bound(DeliveryKind::Youtube).await.unwrap(), Some(3));

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_page_shows_paused_when_the_bound_bot_left(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    register(&pool, 1, Slot::Zayden, "Zayden").await.unwrap();
    register(&pool, 3, Slot::Maria, "Maria").await.unwrap();
    join(&pool, 1).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    DeliveryStore::bind_application(&pool, DeliveryKind::Youtube, 7, Some(3))
        .await
        .unwrap();
    let app = harness(&pool).await.unwrap();

    let html = app.page("/guild/7/youtube").await.unwrap();

    assert!(html.contains("Deliveries paused"), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_page_shows_the_webhook_state(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    connect_youtube(&pool, Some(101)).await.unwrap();
    let app = harness(&pool).await.unwrap();

    let html = app.page("/guild/7/youtube").await.unwrap();
    assert!(html.contains("Webhook not set up yet"), "{html}");

    webhook(&pool, 101, false).await.unwrap();
    let html = app.page("/guild/7/youtube").await.unwrap();
    assert!(html.contains("Posted by Zayden."), "{html}");
    assert!(!html.contains("Webhook not set up yet"), "{html}");

    webhook(&pool, 101, true).await.unwrap();
    let html = app.page("/guild/7/youtube").await.unwrap();
    assert!(html.contains("Webhook not set up yet"), "{html}");

    pool.close().await;
}

#[sqlx::test(migrator = "schema::MIGRATOR")]
async fn the_deadlock_squad_form_binds_its_own_table(
    options: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let pool = test_pool(options, connect).await.unwrap();
    both_bots(&pool).await.unwrap();
    PatchAnnounceRow::upsert(&pool, 7, 101, None).await.unwrap();
    summary_channel::upsert(&pool, 7, 101).await.unwrap();
    squad_channel::upsert(&pool, 7, 101, false).await.unwrap();
    let app = harness(&pool).await.unwrap();

    app.saved(
        "/guild/7/deadlock/squad",
        "guild=7&squad_channel_id=101&squad_leaderboard=&delivery_application_id=3",
    )
    .await
    .unwrap();

    assert_eq!(app.bound(DeliveryKind::DeadlockSquad).await.unwrap(), Some(3));
    assert_eq!(app.bound(DeliveryKind::DeadlockPatch).await.unwrap(), None);
    assert_eq!(app.bound(DeliveryKind::DeadlockSummary).await.unwrap(), None);

    pool.close().await;
}
