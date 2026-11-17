use topcoat::router::segment;
mod access;
mod error;
mod fields;
mod kofi;
pub(crate) mod oauth;
mod patreon;
mod reply;
mod youtube;

segment!(kind: Group);
