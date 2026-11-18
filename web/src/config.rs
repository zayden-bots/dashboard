use std::env;
use std::net::{AddrParseError, IpAddr, SocketAddr};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use topcoat::cookie::Key;
use topcoat::router::IpNet;
use zayden_app::config::BotConfig;

use crate::error::{CookieKeyError, StartupError};

pub const BIND_ADDR_ENV: &str = "DASHBOARD_BIND_ADDR";
pub const COOKIE_KEY_ENV: &str = "DASHBOARD_COOKIE_KEY";
pub const COOKIE_KEY_BYTES: usize = 64;
pub const TRUSTED_PROXIES_ENV: &str = "DASHBOARD_TRUSTED_PROXIES";

pub fn bind_addr(config: &BotConfig) -> Result<SocketAddr, AddrParseError> {
    resolve_bind_addr(env::var(BIND_ADDR_ENV).ok().as_deref(), &config.bind_addr)
}

pub fn resolve_bind_addr(
    env_override: Option<&str>,
    configured: &str,
) -> Result<SocketAddr, AddrParseError> {
    env_override
        .map(str::trim)
        .filter(|addr| !addr.is_empty())
        .unwrap_or(configured)
        .parse()
}

pub fn trusted_proxies() -> Result<Vec<IpNet>, StartupError> {
    parse_trusted_proxies(env::var(TRUSTED_PROXIES_ENV).ok().as_deref())
}

pub fn parse_trusted_proxies(
    value: Option<&str>,
) -> Result<Vec<IpNet>, StartupError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .parse::<IpNet>()
                .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_invalid| StartupError::TrustedProxy(entry.to_owned()))
        })
        .collect()
}

pub fn cookie_key() -> Result<Option<Key>, CookieKeyError> {
    parse_cookie_key(env::var(COOKIE_KEY_ENV).ok().as_deref())
}

pub fn parse_cookie_key(value: Option<&str>) -> Result<Option<Key>, CookieKeyError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let bytes = STANDARD.decode(value)?;
    Key::try_from(bytes.as_slice())
        .map(Some)
        .map_err(|_short| CookieKeyError::TooShort(bytes.len()))
}
