//! Sanitized presentation errors; source payloads never become diagnostics.

use serde_json::{Value, json};

pub(crate) type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug)]
pub(crate) struct Failure {
    pub code: &'static str,
    pub message: String,
    pub exit: u8,
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl Failure {
    pub fn query(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_query",
            message: message.into(),
            exit: 2,
            cause: None,
        }
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self {
            code: "storage_error",
            message: message.into(),
            exit: 3,
            cause: None,
        }
    }

    pub fn json(&self) -> Value {
        json!({"schema_version":1,"error":{"code":self.code,"message":self.message}})
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        let mut failure =
            Self::storage(format!("Local file operation failed ({:?})", error.kind()));
        failure.cause = Some(Box::new(error));
        failure
    }
}

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        let mut failure = Self::storage(format!(
            "SQLite operation failed ({:?}); check index/source availability and schema",
            error.sqlite_error_code()
        ));
        failure.cause = Some(Box::new(error));
        failure
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        let mut failure = Self::storage("Malformed JSON record; source content omitted");
        failure.cause = Some(Box::new(error));
        failure
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|error| error.as_ref() as &dyn std::error::Error)
    }
}
