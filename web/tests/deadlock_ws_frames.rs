use std::time::Duration;

use deadlock_protocol::ws::{MAX_FRAME_BYTES, MAX_UP_FRAMES_PER_SEC};
use tokio::time::Instant;
use web::deadlock_app::ws::{FrameRate, Verdict, judge};

const ACCOUNT: u32 = 77;

fn hello(app_version: &str) -> String {
    serde_json::json!({
        "type": "hello",
        "app_version": app_version,
        "client_version": null,
    })
    .to_string()
}

fn game(event: &serde_json::Value) -> String {
    serde_json::json!({ "type": "game", "at_ms": 1, "match_time_ms": null, "event": event })
        .to_string()
}

const fn policy(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Policy(_))
}

#[test]
fn a_hello_is_accepted() {
    assert_eq!(judge(&hello("0.1.0"), ACCOUNT, false), Verdict::Hello);
}

#[test]
fn a_long_app_version_is_policy() {
    assert!(policy(&judge(&hello(&"v".repeat(65)), ACCOUNT, false)));
}

#[test]
fn a_game_frame_first_is_policy() {
    let frame = game(&serde_json::json!({ "kind": "phase", "phase": "hideout" }));
    assert!(policy(&judge(&frame, ACCOUNT, false)));
}

#[test]
fn garbage_first_is_policy() {
    assert!(policy(&judge("not json", ACCOUNT, false)));
}

#[test]
fn garbage_after_hello_is_an_error() {
    assert!(matches!(judge("not json", ACCOUNT, true), Verdict::Error(_)));
}

#[test]
fn a_duplicate_hello_is_an_error() {
    assert!(matches!(judge(&hello("0.1.0"), ACCOUNT, true), Verdict::Error(_)));
}

#[test]
fn an_unknown_type_after_hello_is_ignored() {
    assert_eq!(judge(r#"{"type":"later"}"#, ACCOUNT, true), Verdict::Ignore);
}

#[test]
fn names_are_capped_at_64_bytes() {
    let name = |kind: &str, len: usize| match kind {
        "hero" => serde_json::json!({ "kind": "hero", "name": "h".repeat(len) }),
        "map" => serde_json::json!({ "kind": "map", "name": "m".repeat(len) }),
        _ => {
            serde_json::json!({ "kind": "cue", "name": "c".repeat(len), "confidence": 0.5 })
        },
    };
    for kind in ["hero", "map", "cue"] {
        assert!(policy(&judge(&game(&name(kind, 65)), ACCOUNT, true)), "{kind}");
        assert_eq!(
            judge(&game(&name(kind, 64)), ACCOUNT, true),
            Verdict::Game,
            "{kind}"
        );
    }
}

#[test]
fn a_roster_is_capped_at_six() {
    let roster = |len: usize| serde_json::json!({ "kind": "roster", "enemies": vec![1_u32; len] });
    assert!(policy(&judge(&game(&roster(7)), ACCOUNT, true)));
    assert_eq!(judge(&game(&roster(6)), ACCOUNT, true), Verdict::Game);
}

#[test]
fn another_account_is_a_mismatch() {
    let account = |id: u32| serde_json::json!({ "kind": "account", "id": id });
    assert_eq!(
        judge(&game(&account(ACCOUNT + 1)), ACCOUNT, true),
        Verdict::AccountMismatch
    );
    assert_eq!(judge(&game(&account(ACCOUNT)), ACCOUNT, true), Verdict::Game);
}

#[test]
fn an_oversized_frame_is_policy() {
    let text = " ".repeat(MAX_FRAME_BYTES + 1);
    assert!(policy(&judge(&text, ACCOUNT, true)));
}

#[test]
fn the_frame_rate_admits_twenty_a_second() {
    let mut rate = FrameRate::new();
    let now = Instant::now();
    for _ in 0..MAX_UP_FRAMES_PER_SEC {
        assert!(rate.admit(now));
    }
    assert!(!rate.admit(now));
    assert!(rate.admit(now + Duration::from_secs(1)));
}
