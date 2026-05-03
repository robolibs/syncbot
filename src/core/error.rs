//! Crate-wide `Error` and `Result`. Mirrors `dp::Error::*` variants used by
//! the C++ side: `invalid_argument`, `not_found`, `parse_error`.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("zoneout: {0}")]
    Zoneout(#[from] zoneout::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Msg(String),
}

impl Error {
    pub fn invalid_argument(msg: impl Into<String>) -> Self {
        Self::InvalidArgument(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn parse(msg: impl Into<String>) -> Self {
        Self::Parse(msg.into())
    }

    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Msg(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
