use thiserror::Error;

#[derive(Debug, Error)]
pub enum LclError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Msg(String),
}
pub type Result<T> = std::result::Result<T, LclError>;
