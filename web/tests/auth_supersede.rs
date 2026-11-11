//! Concurrent writes to one guild module collapse to the last request.

use twilight_model::id::Id;
use web::auth::supersede::claim;
use zayden_app::applications::Slot;

#[test]
fn a_lone_claim_is_not_superseded() {
    let first = claim(Id::new(1001), Slot::Zayden, "levels");

    assert!(!first.superseded());
}

#[test]
fn a_newer_claim_supersedes_an_older_one() {
    let first = claim(Id::new(1002), Slot::Zayden, "levels");
    let second = claim(Id::new(1002), Slot::Zayden, "levels");

    assert!(first.superseded());
    assert!(!second.superseded());
}

#[test]
fn claims_on_other_modules_seats_or_guilds_are_independent() {
    let first = claim(Id::new(1003), Slot::Zayden, "levels");
    let _other_module = claim(Id::new(1003), Slot::Zayden, "music");
    let _other_seat = claim(Id::new(1003), Slot::Maria, "levels");
    let _other_guild = claim(Id::new(1004), Slot::Zayden, "levels");

    assert!(!first.superseded());
}

/// Once every claim is dropped the slot is gone, so the next claim starts
/// fresh instead of comparing against a stale ticket.
#[test]
fn dropping_the_latest_claim_clears_the_slot() {
    let first = claim(Id::new(1005), Slot::Zayden, "levels");
    drop(first);

    let next = claim(Id::new(1005), Slot::Zayden, "levels");
    assert!(!next.superseded());
}

#[tokio::test]
async fn writes_to_one_module_take_turns() {
    let first = claim(Id::new(1006), Slot::Zayden, "levels");
    let second = claim(Id::new(1006), Slot::Zayden, "levels");

    let blocked = {
        let _turn = first.wait_for_turn().await;
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            second.wait_for_turn(),
        )
        .await
        .is_err()
    };
    assert!(blocked);

    let _turn = second.wait_for_turn().await;
    assert!(first.superseded());
}
