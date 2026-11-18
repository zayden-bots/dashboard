use deadlock_protocol::PROTOCOL_VERSION;
use deadlock_protocol::error::{ApiError, ErrorCode};
use topcoat::context::Cx;
use topcoat::router::content::Json;
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{HeaderValue, StatusCode, header};

#[derive(Debug, thiserror::Error)]
pub enum AppApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("missing, malformed or revoked access token")]
    Unauthorized,
    #[error("no such route")]
    NotFound,
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("too many requests, retry in {retry_after_secs} s")]
    RateLimited { retry_after_secs: u32 },
    #[error("x-deadlock-protocol must be {PROTOCOL_VERSION}")]
    ProtocolUnsupported,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("accounts error: {0}")]
    Accounts(#[from] accounts::AccountsError),
    #[error(transparent)]
    Auth(#[from] crate::auth::AuthError),
}

impl AppApiError {
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::BadRequest(_) | Self::MethodNotAllowed => ErrorCode::BadRequest,
            Self::Unauthorized => ErrorCode::Unauthorized,
            Self::NotFound => ErrorCode::NotFound,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::ProtocolUnsupported => ErrorCode::ProtocolUnsupported,
            Self::Database(_) | Self::Accounts(_) | Self::Auth(_) => {
                ErrorCode::Internal
            },
        }
    }

    #[must_use]
    pub fn body(&self) -> ApiError {
        let code = self.code();
        ApiError {
            code,
            message: if code == ErrorCode::Internal {
                "internal error".to_owned()
            } else {
                self.to_string()
            },
            retry_after_secs: self.retry_after_secs(),
        }
    }

    const fn retry_after_secs(&self) -> Option<u32> {
        if let Self::RateLimited { retry_after_secs } = self {
            Some(*retry_after_secs)
        } else {
            None
        }
    }

    pub fn respond(&self, cx: &Cx) -> topcoat::Result<Response> {
        let code = self.code();
        if code == ErrorCode::Internal {
            tracing::warn!(error = %self, "deadlock app API request failed");
        }
        let status = StatusCode::from_u16(code.status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(self.body())).into_response(cx)?;
        if let Some(retry_after_secs) = self.retry_after_secs() {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
        }
        Ok(response)
    }
}
