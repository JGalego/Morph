use thiserror::Error;

/// The single error type crossing every trait boundary in Morph.
///
/// Every crate that implements a core trait (`ProviderAdapter`, `Renderer`, ...)
/// converts its internal errors into one of these variants so callers never need
/// to know which concrete adapter/renderer/plugin produced a failure.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("unsupported: {0}")]
    Unsupported(String),

    #[error("upstream provider error (status {status:?}): {message}")]
    Upstream {
        status: Option<u16>,
        message: String,
    },

    #[error("provider timed out after {0}ms")]
    Timeout(u64),

    #[error("render error: {0}")]
    Render(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("plugin error: {0}")]
    Plugin(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("authentication failed: {0}")]
    Unauthorized(String),

    #[error("rate limited, retry after {retry_after_ms:?}ms")]
    RateLimited { retry_after_ms: Option<u64> },

    #[error("internal error: {0}")]
    Internal(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl GatewayError {
    /// Best-effort HTTP status code for this error, used by protocol adapters
    /// that need to surface failures in the client's expected wire shape.
    pub fn status_code(&self) -> u16 {
        match self {
            GatewayError::InvalidRequest(_) => 400,
            GatewayError::Unauthorized(_) => 401,
            GatewayError::RateLimited { .. } => 429,
            GatewayError::Unsupported(_) => 400,
            GatewayError::Timeout(_) => 504,
            GatewayError::Upstream { status, .. } => status.unwrap_or(502),
            GatewayError::Render(_) | GatewayError::Protocol(_) | GatewayError::Plugin(_) => 500,
            GatewayError::Config(_) => 500,
            GatewayError::Internal(_) | GatewayError::Io(_) | GatewayError::Json(_) => 500,
        }
    }
}

pub type Result<T> = std::result::Result<T, GatewayError>;
