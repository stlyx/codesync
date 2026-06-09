use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CodeSyncError>;

#[derive(Debug, Error)]
pub enum CodeSyncError {
    #[error("config error: {0}")]
    Config(String),
    #[error("unsupported configuration: {0}")]
    Unsupported(String),
    #[error("credential error: {0}")]
    Credential(String),
    #[error("sync conflict: {0}")]
    Conflict(String),
    #[error("git backend error: {0}")]
    GitBackend(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl CodeSyncError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Conflict(_) => 409,
            Self::Http(message) if message == "unauthorized" => 401,
            Self::Http(message) if message == "payload_too_large" => 413,
            Self::Http(message) if message == "not_found" => 404,
            _ => 500,
        }
    }

    pub fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_error_display_is_prefixed() {
        let error: CodeSyncError = serde_json::from_str::<serde_json::Value>("{")
            .expect_err("invalid JSON should fail")
            .into();

        assert!(
            error.to_string().starts_with("json error: "),
            "unexpected display message: {error}"
        );
    }
}
