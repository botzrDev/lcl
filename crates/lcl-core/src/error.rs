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
    /// No issue behind a well-formed reference. Holds its `KEY-N` (or the
    /// UUID, when that is what was given).
    #[error("issue {0} not found")]
    IssueNotFound(String),
    /// `KEY-N` with a key that isn't this workspace's.
    #[error("{given} is not in this workspace (key {key})")]
    WrongWorkspace { given: String, key: String },
    #[error("{0} can't be its own parent")]
    SelfParent(String),
    /// `path` is the loop the new link would close, from the issue back to it.
    #[error("making {parent} the parent of {issue} would make a cycle: {path}")]
    ParentCycle { issue: String, parent: String, path: String },
    #[error("{issue} has sub-issues {children}; reparent or delete them first")]
    HasChildren { issue: String, children: String },
    /// A label argument naming no label.
    #[error("unknown label {0:?}; create it with: lcl label create {word}", word = shell_word(.0))]
    UnknownLabel(String),
    /// `label create` with a name that exists, ignoring case. `existing` is
    /// the stored spelling.
    #[error("label {name:?} already exists{}", if name == existing { String::new() } else { format!(" (as {existing:?})") })]
    LabelExists { name: String, existing: String },
    /// `label delete` of a name or id that names no label.
    #[error("label {0:?} not found")]
    LabelNotFound(String),
    #[error("doc {0} not found")]
    DocNotFound(String),
    #[error("the search query is empty")]
    EmptyQuery,
    /// Every word lacks token characters (`--`, `*`, a lone `"`).
    #[error("the search query {0:?} has no words to search for")]
    NothingToSearch(String),
    #[error("{0}")]
    Msg(String),
}
pub type Result<T> = std::result::Result<T, LclError>;

impl LclError {
    /// The request itself is wrong, whatever the data: an issue from another
    /// workspace, or a parent link that can never be valid. A CLI reports
    /// these as usage errors (exit 2). Everything else is about the state of
    /// the data or the system.
    pub fn is_usage(&self) -> bool {
        matches!(
            self,
            LclError::WrongWorkspace { .. }
                | LclError::SelfParent(_)
                | LclError::ParentCycle { .. }
                | LclError::UnknownLabel(_)
                | LclError::EmptyQuery
                | LclError::NothingToSearch(_)
        )
    }
}

/// `s` as one shell word: as is when that's safe, else single-quoted.
fn shell_word(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._/:@%+=,-".contains(c));
    if plain { s.to_string() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}
