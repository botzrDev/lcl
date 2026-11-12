use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Canceled,
}
impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Backlog => "Backlog",
            Status::Todo => "Todo",
            Status::InProgress => "InProgress",
            Status::InReview => "InReview",
            Status::Done => "Done",
            Status::Canceled => "Canceled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Backlog" => Some(Status::Backlog),
            "Todo" => Some(Status::Todo),
            "InProgress" => Some(Status::InProgress),
            "InReview" => Some(Status::InReview),
            "Done" => Some(Status::Done),
            "Canceled" => Some(Status::Canceled),
            _ => None,
        }
    }
    pub fn all() -> &'static [Status] {
        &[Status::Backlog, Status::Todo, Status::InProgress, Status::InReview, Status::Done, Status::Canceled]
    }
}
impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.as_str()) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    None = 0,
    Urgent = 1,
    High = 2,
    Medium = 3,
    Low = 4,
}
impl Priority {
    pub fn as_i64(&self) -> i64 { *self as i64 }
    pub fn from_i64(v: i64) -> Self {
        match v { 1 => Priority::Urgent, 2 => Priority::High, 3 => Priority::Medium, 4 => Priority::Low, _ => Priority::None }
    }
    pub fn label(&self) -> &'static str {
        match self { Priority::None => "None", Priority::Urgent => "Urgent", Priority::High => "High", Priority::Medium => "Medium", Priority::Low => "Low" }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label { pub id: String, pub name: String, pub color: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub number: i64,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: Priority,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
    pub estimate: Option<i64>,
    pub due_date: Option<String>,
    pub labels: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub issue_id: String,
    pub body: String,
    pub author: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doc {
    pub id: String,
    pub title: String,
    pub content: String,
    pub parent_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cycle {
    pub id: String,
    pub name: String,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub key: String,
    pub description: String,
    pub created_at: i64,
}

pub fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
