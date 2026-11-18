pub mod auth;
pub mod device;
pub mod error;
pub mod limits;
pub mod proxy;
pub mod reply;
pub mod tokens;
pub mod ws;

pub use error::AppApiError;

pub const LINK_APP_PATH: &str = "/link/app";
