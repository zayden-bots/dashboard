use std::sync::Arc;

use deadlock_protocol::ws::{
    CLOSE_ACCOUNT_MISMATCH,
    CLOSE_POLICY,
    CLOSE_UNAUTHORIZED,
    Down,
};
use web::deadlock_app::ws::{Mailbox, WsHub, WsTiming};

const TOKEN: i64 = 7;
const USER: i64 = 41;
const ACCOUNT: u32 = 22_202;

fn hub() -> Arc<WsHub> {
    Arc::new(WsHub::new(WsTiming::default()))
}

fn flagged(mailbox: &Mailbox) -> Option<u16> {
    *mailbox.close.borrow()
}

#[test]
fn a_full_outbound_queue_closes_the_socket() {
    let hub = hub();
    let (_registration, mailbox) = hub.register(TOKEN, USER, ACCOUNT);
    for _ in 0..32 {
        hub.push_user(USER, &Down::Patch { client_version: 1 });
    }
    assert_eq!(flagged(&mailbox), None);
    hub.push_user(USER, &Down::Patch { client_version: 1 });
    assert_eq!(flagged(&mailbox), Some(CLOSE_POLICY));
}

#[test]
fn the_token_cap_evicts_the_oldest_live_sockets() {
    let hub = hub();
    let sockets: Vec<_> =
        (0..4).map(|_| hub.register(TOKEN, USER, ACCOUNT)).collect();
    assert_eq!(flagged(&sockets[0].1), Some(CLOSE_POLICY));
    assert_eq!(flagged(&sockets[1].1), None);

    let fifth = hub.register(TOKEN, USER, ACCOUNT);
    assert_eq!(flagged(&sockets[1].1), Some(CLOSE_POLICY));
    assert_eq!(flagged(&sockets[2].1), None);
    assert_eq!(flagged(&sockets[3].1), None);
    assert_eq!(flagged(&fifth.1), None);
    assert_eq!(hub.len(), 5);
}

#[test]
fn dropping_a_registration_removes_the_socket() {
    let hub = hub();
    let (registration, _mailbox) = hub.register(TOKEN, USER, ACCOUNT);
    assert_eq!(hub.len(), 1);
    drop(registration);
    assert!(hub.is_empty());
}

#[test]
fn the_first_close_code_wins() {
    let hub = hub();
    let (registration, mailbox) = hub.register(TOKEN, USER, ACCOUNT);
    hub.close_token(TOKEN, CLOSE_UNAUTHORIZED);
    hub.close_socket(registration.id(), CLOSE_ACCOUNT_MISMATCH);
    assert_eq!(flagged(&mailbox), Some(CLOSE_UNAUTHORIZED));
}
