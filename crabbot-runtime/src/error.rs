use axum::{http::StatusCode, response::IntoResponse};
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
pub enum Error {
    ConfigError(String),
    IOError(String),
    Other(String),
    Unauthorized(String),
    BadRequest(String),
    NotFound(String),
    LLMError(String),
    ToolError(String),
}

impl Error {
    pub fn config(msg: impl Into<String>) -> Self {
        Error::ConfigError(msg.into())
    }

    pub fn io(msg: impl Into<String>) -> Self {
        Error::IOError(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Error::Unauthorized(msg.into())
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Error::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }

    pub fn llm(msg: impl Into<String>) -> Self {
        Error::LLMError(msg.into())
    }

    pub fn tool(msg: impl Into<String>) -> Self {
        Error::ToolError(msg.into())
    }

    pub fn session_not_found(msg: impl Into<String>) -> Self {
        Error::Other(format!("Session not found: {}", msg.into()))
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::IOError(err.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized(m) => write!(f, "Unauthorized: {}", m),
            Error::BadRequest(m) => write!(f, "Bad Request: {}", m),
            Error::NotFound(m) => write!(f, "Not Found: {}", m),
            Error::Other(m)
            | Error::ConfigError(m)
            | Error::IOError(m)
            | Error::LLMError(m)
            | Error::ToolError(m) => write!(f, "Other Error: {}", m),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match self {
            Error::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m.to_string()),
            Error::BadRequest(m) => (StatusCode::BAD_REQUEST, m.to_string()),
            Error::NotFound(m) => (StatusCode::NOT_FOUND, m.to_string()),
            Error::Other(m)
            | Error::ConfigError(m)
            | Error::IOError(m)
            | Error::LLMError(m)
            | Error::ToolError(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };

        tracing::error!("Error: {}", msg);

        (status, axum::Json(json!({ "error": msg }))).into_response()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn init_tracing() {
    // If RUST_LOG is unset, default to info.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Ignore "already initialized" (common in tests / multiple init paths).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .try_init();
}

pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Log best-effort into tracing (if initialized).
        if let Some(loc) = info.location() {
            tracing::error!(
                file = loc.file(),
                line = loc.line(),
                message = %info,
                "panic"
            );
        } else {
            tracing::error!(message = %info, "panic");
        }

        // Always keep default behavior (stderr + optional backtrace via RUST_BACKTRACE).
        default(info);
    }));
}
