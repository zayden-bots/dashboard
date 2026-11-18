use std::net::SocketAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use topcoat::cookie::Key;
use web::config::{
    COOKIE_KEY_BYTES,
    parse_cookie_key,
    parse_trusted_proxies,
    resolve_bind_addr,
};
use web::error::{CookieKeyError, StartupError};

#[test]
fn configured_address_is_used_without_override() {
    let addr = resolve_bind_addr(None, "0.0.0.0:3000").unwrap();
    assert_eq!(addr, "0.0.0.0:3000".parse::<SocketAddr>().unwrap());
}

#[test]
fn env_override_wins() {
    let addr = resolve_bind_addr(Some("127.0.0.1:3100"), "0.0.0.0:3000").unwrap();
    assert_eq!(addr, "127.0.0.1:3100".parse::<SocketAddr>().unwrap());
}

#[test]
fn blank_override_falls_back_to_config() {
    let addr = resolve_bind_addr(Some("  "), "0.0.0.0:3000").unwrap();
    assert_eq!(addr, "0.0.0.0:3000".parse::<SocketAddr>().unwrap());
}

#[test]
fn invalid_override_is_an_error() {
    assert!(resolve_bind_addr(Some("localhost"), "0.0.0.0:3000").is_err());
}

#[test]
fn an_unset_cookie_key_falls_back() {
    assert_eq!(parse_cookie_key(None).unwrap(), None);
    assert_eq!(parse_cookie_key(Some(" ")).unwrap(), None);
}

#[test]
fn a_configured_cookie_key_is_stable_across_loads() {
    let secret = STANDARD.encode([7_u8; COOKIE_KEY_BYTES]);
    let first = parse_cookie_key(Some(&secret)).unwrap().unwrap();
    let second = parse_cookie_key(Some(&secret)).unwrap().unwrap();
    assert_eq!(first, second);
    assert_eq!(first, Key::from(&[7_u8; COOKIE_KEY_BYTES]));
}

#[test]
fn unset_or_blank_trusted_proxies_are_empty() {
    assert_eq!(parse_trusted_proxies(None).unwrap(), Vec::new());
    assert_eq!(parse_trusted_proxies(Some("  ")).unwrap(), Vec::new());
}

#[test]
fn trusted_proxies_accept_networks_and_single_addresses() {
    let nets = parse_trusted_proxies(Some("10.0.0.0/8, 192.0.2.7")).unwrap();
    assert_eq!(nets.len(), 2);
    assert_eq!(nets[0].to_string(), "10.0.0.0/8");
    assert_eq!(nets[1].to_string(), "192.0.2.7/32");
}

#[test]
fn a_malformed_trusted_proxy_is_an_error() {
    assert!(matches!(
        parse_trusted_proxies(Some("nope")),
        Err(StartupError::TrustedProxy(_))
    ));
}

#[test]
fn a_short_or_malformed_cookie_key_is_an_error() {
    let short = STANDARD.encode([7_u8; 32]);
    assert!(matches!(
        parse_cookie_key(Some(&short)),
        Err(CookieKeyError::TooShort(32))
    ));
    assert!(matches!(
        parse_cookie_key(Some("not base64!")),
        Err(CookieKeyError::Base64(_))
    ));
}
