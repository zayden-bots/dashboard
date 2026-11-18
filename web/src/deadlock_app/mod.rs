pub mod auth;
pub mod device;
pub mod error;
pub mod limits;
pub mod reply;
pub mod tokens;

pub use error::AppApiError;

pub const LINK_APP_PATH: &str = "/link/app";
