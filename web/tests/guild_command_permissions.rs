//! Building Discord's command-permission arrays.
//!
//! Discord only accepts a whole-array overwrite, so every writer is a
//! read-modify-write and every writer can therefore destroy settings it does
//! not own. Two pages write to the same array — the modules page owns the
//! `@everyone` switch, a feature's own page owns the channel entries — and an
//! admin may have added user overrides in Discord that belong to neither.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use twilight_http::Client;
use twilight_model::application::command::permissions::{
    CommandPermission,
    CommandPermissionType,
};
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker};
use web::auth::GuildAccess;
use web::guild::GuildError;
use web::guild::command_permissions::{
    GuildContext,
    all_channels,
    channel_allowlist,
    everyone,
    everyone_denied,
    read_rejection,
    sign_in_rejected,
    store,
    with_channel_allowlist,
    with_everyone_denied,
    write_rejection,
};

/// `channel_allowlist` returns `Vec<Id<ChannelMarker>>`; this names the empty
/// case so the assertions can compare against it without spelling the type out.
const NO_CHANNELS: [Id<ChannelMarker>; 0] = [];

const GUILD: Id<GuildMarker> = Id::new(900);
const GENERAL: Id<ChannelMarker> = Id::new(11);
const OFF_TOPIC: Id<ChannelMarker> = Id::new(12);

/// Written out by hand rather than via `all_channels`, so the sentinel test
/// below compares the implementation against Discord's documented encoding
/// instead of against itself.
const ALL_CHANNELS: Id<ChannelMarker> = Id::new(899);

const fn deny_all_channels() -> CommandPermission {
    CommandPermission {
        id: CommandPermissionType::Channel(ALL_CHANNELS),
        permission: false,
    }
}

const fn allow_channel(id: Id<ChannelMarker>) -> CommandPermission {
    CommandPermission { id: CommandPermissionType::Channel(id), permission: true }
}

const fn deny_everyone() -> CommandPermission {
    CommandPermission {
        id: CommandPermissionType::Role(everyone(GUILD)),
        permission: false,
    }
}

const fn allow_user(id: u64) -> CommandPermission {
    CommandPermission {
        id: CommandPermissionType::User(Id::new(id)),
        permission: true,
    }
}

#[test]
fn the_all_channels_sentinel_is_the_guild_id_minus_one() {
    // Off by one in either direction and the deny lands on a real channel, or
    // on nothing, and the restriction silently stops applying.
    assert_eq!(all_channels(GUILD), Some(ALL_CHANNELS));
}

#[test]
fn no_overwrites_means_no_restriction() {
    assert_eq!(channel_allowlist(GUILD, &[]), NO_CHANNELS);
    assert!(!everyone_denied(GUILD, &[]));
}

#[test]
fn allows_without_the_deny_all_are_not_a_restriction() {
    // Discord treats a bare allow as a no-op: the command already ran there.
    // Reporting it as an allowlist would show a restriction that is not on.
    assert_eq!(channel_allowlist(GUILD, &[allow_channel(GENERAL)]), NO_CHANNELS);
}

#[test]
fn the_allowlist_is_the_allows_alongside_the_deny_all() {
    let perms =
        [deny_all_channels(), allow_channel(GENERAL), allow_channel(OFF_TOPIC)];

    assert_eq!(channel_allowlist(GUILD, &perms), vec![GENERAL, OFF_TOPIC]);
}

#[test]
fn the_everyone_switch_is_read_independently_of_channels() {
    let perms = [deny_everyone(), deny_all_channels(), allow_channel(GENERAL)];

    assert!(everyone_denied(GUILD, &perms));
    assert_eq!(channel_allowlist(GUILD, &perms), vec![GENERAL]);
}

#[test]
fn setting_an_allowlist_writes_the_deny_all_plus_one_allow_each() {
    let out = with_channel_allowlist(GUILD, &[], &[GENERAL, OFF_TOPIC]);

    assert!(
        out.contains(&deny_all_channels()),
        "without the deny-all, the \
         allows do nothing and the command stays open everywhere"
    );
    assert!(out.contains(&allow_channel(GENERAL)));
    assert!(out.contains(&allow_channel(OFF_TOPIC)));
    assert_eq!(out.len(), 3);
}

#[test]
fn an_empty_allowlist_clears_the_restriction_entirely() {
    let current = [deny_all_channels(), allow_channel(GENERAL)];
    let out = with_channel_allowlist(GUILD, &current, &[]);

    assert!(
        out.is_empty(),
        "dropping the last channel must also drop the deny-all, or /good \
         would be left runnable nowhere"
    );
}

#[test]
fn writing_channels_preserves_the_module_switch_and_user_overrides() {
    let current = [deny_everyone(), allow_user(7), deny_all_channels()];
    let out = with_channel_allowlist(GUILD, &current, &[GENERAL]);

    assert!(
        out.contains(&deny_everyone()),
        "editing channels must not silently re-enable a disabled module"
    );
    assert!(out.contains(&allow_user(7)));
    assert_eq!(channel_allowlist(GUILD, &out), vec![GENERAL]);
}

#[test]
fn the_sentinel_cannot_be_added_as_an_ordinary_channel() {
    let out = with_channel_allowlist(GUILD, &[], &[ALL_CHANNELS, GENERAL]);

    assert_eq!(
        out.iter().filter(|p| **p == deny_all_channels()).count(),
        1,
        "an allow on the sentinel would contradict the deny on the same id"
    );
    assert_eq!(channel_allowlist(GUILD, &out), vec![GENERAL]);
}

#[test]
fn disabling_a_module_preserves_the_channel_restriction() {
    let current = [deny_all_channels(), allow_channel(GENERAL)];
    let out = with_everyone_denied(GUILD, &current, true);

    assert!(everyone_denied(GUILD, &out));
    assert_eq!(
        channel_allowlist(GUILD, &out),
        vec![GENERAL],
        "toggling a module off must not wipe where its commands are allowed"
    );
}

#[test]
fn re_enabling_a_module_preserves_the_channel_restriction() {
    let current = [deny_everyone(), deny_all_channels(), allow_channel(GENERAL)];
    let out = with_everyone_denied(GUILD, &current, false);

    assert!(!everyone_denied(GUILD, &out));
    assert_eq!(channel_allowlist(GUILD, &out), vec![GENERAL]);
}

#[test]
fn the_everyone_entry_is_never_duplicated() {
    let out = with_everyone_denied(GUILD, &[deny_everyone()], true);

    assert_eq!(out, vec![deny_everyone()]);
}

#[test]
fn sign_in_rejections_are_recognised() {
    assert!(sign_in_rejected(401, None));
    assert!(sign_in_rejected(403, None));
    assert!(sign_in_rejected(400, Some(50001)));
    assert!(sign_in_rejected(400, Some(50013)));
    assert!(sign_in_rejected(400, Some(20012)));
}

#[test]
fn other_failures_are_not_sign_in_rejections() {
    assert!(!sign_in_rejected(400, Some(50035)));
    assert!(!sign_in_rejected(500, None));
}

#[expect(clippy::unwrap_used, reason = "test helper: a failed bind is a test error")]
fn discord_answering(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let _ = answer(stream, status, body);
            });
        }
    });
    addr
}

fn answer(stream: TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

#[expect(clippy::unwrap_used, reason = "test helper: the request must fail")]
async fn refused(status: u16, body: &'static str) -> twilight_http::Error {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let client = Client::builder()
        .token("Bot x".to_owned())
        .proxy(discord_answering(status, body), true)
        .ratelimiter(None)
        .build();
    client.current_user().await.unwrap_err()
}

async fn refused_write(status: u16, body: &'static str) -> GuildError {
    write_rejection(&refused(status, body).await, "good", "Zayden", true)
}

#[tokio::test]
async fn a_missing_access_write_on_zayden_keeps_the_rejection_text() {
    let error = write_rejection(
        &refused(403, r#"{"message":"Missing Access","code":50001}"#).await,
        "good",
        "Zayden",
        false,
    );

    assert!(matches!(error, GuildError::PermissionUpdateRejected { .. }));
}

#[tokio::test]
async fn a_refused_cross_app_read_shows_the_fallback() {
    let error = read_rejection(
        &refused(403, r#"{"message":"Missing Access","code":50001}"#).await,
        "Maria",
        true,
    );

    assert!(matches!(error, GuildError::PermissionsNotAccepted { .. }));
}

#[tokio::test]
async fn a_refused_read_on_zayden_stays_a_server_error() {
    let error = read_rejection(
        &refused(403, r#"{"message":"Missing Access","code":50001}"#).await,
        "Zayden",
        false,
    );

    assert!(matches!(error, GuildError::Server(_)));
}

#[tokio::test]
async fn a_missing_access_write_shows_the_fallback() {
    let error =
        refused_write(403, r#"{"message":"Missing Access","code":50001}"#).await;

    assert!(matches!(error, GuildError::PermissionsNotAccepted { .. }));
    assert!(
        error
            .to_string()
            .contains("Discord didn't accept the dashboard's sign-in for Zayden")
    );
}

#[tokio::test]
async fn an_unauthorized_write_shows_the_fallback() {
    let error =
        refused_write(401, r#"{"message":"401: Unauthorized","code":0}"#).await;

    assert!(matches!(error, GuildError::PermissionsNotAccepted { .. }));
}

#[tokio::test]
async fn an_invalid_body_keeps_the_rejection_text() {
    let error =
        refused_write(400, r#"{"message":"Invalid Form Body","code":50035}"#).await;

    assert!(matches!(error, GuildError::PermissionUpdateRejected { .. }));
}

#[tokio::test]
async fn operator_access_cannot_write_command_permissions() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let ctx = GuildContext {
        guild_id: GUILD,
        access_token: String::new(),
        http: Arc::new(Client::new(String::new())),
        app_id: Id::new(1),
        access: GuildAccess::Operator,
        bot_name: "Zayden".to_owned(),
        cross_app: false,
    };

    let error = store(&ctx, Id::new(5), "good", &[]).await.unwrap_err();

    assert_eq!(
        error.to_string(),
        "Discord only lets a member with Manage Server change command \
         permissions, so /good can't be changed through operator access."
    );
}
