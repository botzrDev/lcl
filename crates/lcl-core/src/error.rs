use thiserror::Error;

#[derive(Debug, Error)]
pub enum LclError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("database schema v{found} is newer than this lcl supports (v{supported}); upgrade lcl")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("issue {number} has status {value:?}, which this lcl doesn't know")]
    UnknownStatus { number: i64, value: String },
    #[error("issue {number} has priority {value}, which this lcl doesn't know (expected 0-4)")]
    UnknownPriority { number: i64, value: i64 },
    #[error("{0}")]
    Msg(String),
}
pub type Result<T> = std::result::Result<T, LclError>;
