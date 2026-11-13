use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// An issue's id, the text stored as `issues.id`. Minted with `new`, parsed
/// from a UUID a person typed, or taken from a row with `from_stored`.
/// A label name or a title does not satisfy this type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueId(String);

impl IssueId {
    /// A new random id, the form lcl stores. There is no `Default`: it
    /// would mint a different id every time.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// A hyphenated UUID, in the lowercase form lcl stores.
    pub fn parse(s: &str) -> Result<Self, InvalidValue> {
        crate::parse::uuid(s).map(Self)
    }

    /// An id already stored. The table's primary key is the check; this does
    /// not re-parse. Tests that build a view without a database use it too.
    pub fn from_stored(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IssueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for IssueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<IssueId> for String {
    fn from(id: IssueId) -> String {
        id.0
    }
}

/// The comparison key for a status a person typed: case folded, and spaces,
/// `_` and `-` dropped. Issue statuses and project statuses share it.
pub(crate) fn loose_key(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect::<String>()
        .to_lowercase()
}

/// A value from the command line that names no known status, priority, etc.
/// Its message is the whole explanation; clap prints it and exits 2.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidValue(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Canceled,
    Duplicate,
}
impl Status {
    /// The canonical spelling: what is stored, and what JSON prints.
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Backlog => "Backlog",
            Status::Todo => "Todo",
            Status::InProgress => "InProgress",
            Status::InReview => "InReview",
            Status::Done => "Done",
            Status::Canceled => "Canceled",
            Status::Duplicate => "Duplicate",
        }
    }
    /// Lenient parse, for command-line input and stored values alike: ignores
    /// case and drops spaces, '_' and '-', so "In Progress", "in_progress" and
    /// "IN-PROGRESS" all work. "cancelled" is accepted for Canceled.
    pub fn parse(s: &str) -> Option<Self> {
        let key = loose_key(s);
        match key.as_str() {
            "backlog" => Some(Status::Backlog),
            "todo" => Some(Status::Todo),
            "inprogress" => Some(Status::InProgress),
            "inreview" => Some(Status::InReview),
            "done" => Some(Status::Done),
            "canceled" | "cancelled" => Some(Status::Canceled),
            "duplicate" => Some(Status::Duplicate),
            _ => None,
        }
    }
    pub fn all() -> &'static [Status] {
        &[
            Status::Backlog,
            Status::Todo,
            Status::InProgress,
            Status::InReview,
            Status::Done,
            Status::Canceled,
            Status::Duplicate,
        ]
    }
}
impl FromStr for Status {
    type Err = InvalidValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Status::parse(s).ok_or_else(|| {
            let expected: Vec<&str> = Status::all().iter().map(Status::as_str).collect();
            InvalidValue(format!(
                "invalid status \"{s}\"; expected one of: {}",
                expected.join(", ")
            ))
        })
    }
}
impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
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
    pub fn as_i64(&self) -> i64 {
        *self as i64
    }
    /// The priority stored as `v`, or None for a value outside 0-4. There is
    /// deliberately no fallback: a value we can't read is not ours to guess.
    pub fn from_stored(v: i64) -> Option<Self> {
        match v {
            0 => Some(Priority::None),
            1 => Some(Priority::Urgent),
            2 => Some(Priority::High),
            3 => Some(Priority::Medium),
            4 => Some(Priority::Low),
            _ => None,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Priority::None => "None",
            Priority::Urgent => "Urgent",
            Priority::High => "High",
            Priority::Medium => "Medium",
            Priority::Low => "Low",
        }
    }
}
/// Accepts 0-4 or a name (none, urgent, high, medium, low), ignoring case.
impl FromStr for Priority {
    type Err = InvalidValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "0" | "none" => Ok(Priority::None),
            "1" | "urgent" => Ok(Priority::Urgent),
            "2" | "high" => Ok(Priority::High),
            "3" | "medium" => Ok(Priority::Medium),
            "4" | "low" => Ok(Priority::Low),
            _ => Err(InvalidValue(format!(
                "invalid priority \"{s}\"; expected 0-4 or one of: none, urgent, high, medium, low"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// An issue as a user names it: parsed, but not yet looked up. Resolving it
/// against a workspace (`Store::resolve`) checks the key and finds the issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueRef {
    /// `N`, `#N` (key `None`) or `KEY-N` (the key as typed; it must equal the
    /// workspace key, ignoring case). `number` is always positive.
    Number { key: Option<String>, number: i64 },
    /// A full UUID, lowercased: the form issue ids are stored in.
    Id(String),
}

impl FromStr for IssueRef {
    type Err = InvalidValue;
    /// Accepts `KEY-N`, `#N`, `N` (N a positive integer) or a hyphenated UUID
    /// in either case. The UUID is tried before `KEY-N`, because a UUID also
    /// ends in "-digits" more often than not.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn positive(digits: &str) -> Option<i64> {
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse::<i64>().ok().filter(|n| *n > 0)
        }
        let number = |key: Option<&str>, n: i64| IssueRef::Number {
            key: key.map(String::from),
            number: n,
        };
        let parsed = if let Some(rest) = s.strip_prefix('#') {
            positive(rest).map(|n| number(None, n))
        } else if let Some(n) = positive(s) {
            Some(number(None, n))
        } else if let Some(id) = (s.len() == 36)
            .then(|| uuid::Uuid::parse_str(s).ok())
            .flatten()
        {
            Some(IssueRef::Id(id.hyphenated().to_string()))
        } else {
            s.rsplit_once('-')
                .filter(|(key, _)| !key.is_empty() && !key.chars().any(char::is_whitespace))
                .and_then(|(key, n)| positive(n).map(|n| number(Some(key), n)))
        };
        parsed.ok_or_else(|| {
            InvalidValue(format!(
                "invalid issue id \"{s}\"; expected KEY-N, #N, N or an issue UUID"
            ))
        })
    }
}

impl std::fmt::Display for IssueRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueRef::Number {
                key: Some(key),
                number,
            } => write!(f, "{key}-{number}"),
            IssueRef::Number { key: None, number } => write!(f, "#{number}"),
            IssueRef::Id(id) => f.write_str(id),
        }
    }
}

/// How an issue is shown to people: `KEY-N`, or `#N` in a workspace with no key.
pub fn identifier(key: &str, number: i64) -> String {
    if key.is_empty() {
        format!("#{number}")
    } else {
        format!("{key}-{number}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    pub number: i64,
    /// `KEY-N`. Derived when the issue is read; ignored on write.
    pub identifier: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: Priority,
    pub assignee: Option<String>,
    /// The parent's id.
    pub parent_id: Option<IssueId>,
    /// The parent's `KEY-N`. Derived when the issue is read; ignored on write.
    pub parent_identifier: Option<String>,
    /// The project's UUID. Set through `db::Placement`, not directly.
    #[serde(skip)]
    pub project_id: Option<String>,
    /// The project's name. Derived when the issue is read; ignored on write.
    pub project: Option<String>,
    /// The milestone's UUID; the milestone is always in the issue's project.
    /// Set through `db::Placement`, not directly.
    #[serde(skip)]
    pub milestone_id: Option<String>,
    /// The milestone's name. Derived when the issue is read; ignored on write.
    pub milestone: Option<String>,
    /// The id the issue has elsewhere (its Linear id, say): on no other
    /// issue, ignoring ASCII case, and never shaped like a reference to one
    /// of this workspace's issues (`N`, `#N`, its `KEY-N`, a UUID).
    pub r#ref: Option<String>,
    /// Who created it. Written on create only.
    pub creator: Option<String>,
    pub estimate: Option<i64>,
    pub due_date: Option<String>,
    /// Label names. Read sorted (ignoring case); on write each must name a
    /// label, matched ignoring case.
    pub labels: Vec<String>,
    /// The cycles it is in, by start date (no date last), then name. Derived
    /// when the issue is read; set through `db::Placement`. Usually one; an
    /// import can give several.
    #[serde(default)]
    pub cycles: Vec<CycleRef>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub started_at: Option<i64>,
    pub canceled_at: Option<i64>,
}

impl Issue {
    /// The lifecycle timestamp rules, for a move from `from` (None when the
    /// issue is being created) to `self.status` at time `at`. Create and
    /// update both call this, inside their write transaction.
    ///
    /// - entering InProgress or InReview sets started_at if it is unset;
    /// - entering Done sets completed_at if unset; leaving Done clears it;
    /// - entering Canceled or Duplicate sets canceled_at if unset; leaving
    ///   both clears it;
    /// - moving to Backlog or Todo clears started_at.
    ///
    /// Staying in a status changes nothing, so an unrelated edit never stamps
    /// a time on an issue whose history predates these columns.
    pub fn apply_lifecycle(&mut self, from: Option<Status>, at: i64) {
        use Status::*;
        let to = self.status;
        if from == Some(to) {
            return;
        }
        let closed = |s: Status| matches!(s, Canceled | Duplicate);
        match to {
            InProgress | InReview => {
                self.started_at.get_or_insert(at);
            }
            Backlog | Todo => self.started_at = None,
            Done | Canceled | Duplicate => {}
        }
        if to == Done {
            self.completed_at.get_or_insert(at);
        } else if from == Some(Done) {
            self.completed_at = None;
        }
        if closed(to) {
            self.canceled_at.get_or_insert(at);
        } else if from.is_some_and(closed) {
            self.canceled_at = None;
        }
    }
}

/// A relation as typed: `A <kind> B`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelationKind {
    /// A blocks B.
    Blocks,
    /// A is blocked by B: stored as B blocks A.
    BlockedBy,
    /// A and B are related; it goes both ways.
    Related,
    /// A is a duplicate of B. An issue is a duplicate of at most one issue.
    DuplicateOf,
}
impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelationKind::Blocks => "blocks",
            RelationKind::BlockedBy => "blocked-by",
            RelationKind::Related => "related",
            RelationKind::DuplicateOf => "duplicate-of",
        }
    }
}
/// Ignores case and drops spaces, '_' and '-', as status names do.
impl FromStr for RelationKind {
    type Err = InvalidValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = loose_key(s);
        match key.as_str() {
            "blocks" => Ok(RelationKind::Blocks),
            "blockedby" => Ok(RelationKind::BlockedBy),
            "related" => Ok(RelationKind::Related),
            "duplicateof" => Ok(RelationKind::DuplicateOf),
            _ => Err(InvalidValue(format!(
                "invalid relation \"{s}\"; expected one of: blocks, blocked-by, related, duplicate-of"
            ))),
        }
    }
}

/// One relation, as typed: `DREA-1 blocked-by DREA-2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relation {
    pub issue: String,
    pub kind: RelationKind,
    pub related: String,
}
impl std::fmt::Display for Relation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}", self.issue, self.kind.as_str(), self.related)
    }
}

/// The other end of a relation, as `issue show` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedIssue {
    pub identifier: String,
    pub title: String,
    pub status: Status,
}

/// Every relation of one issue, seen from that issue. Lists are by number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Relations {
    /// Issues this one blocks.
    pub blocks: Vec<RelatedIssue>,
    /// Issues that block this one.
    pub blocked_by: Vec<RelatedIssue>,
    pub related: Vec<RelatedIssue>,
    /// The issue this one is a duplicate of.
    pub duplicate_of: Option<RelatedIssue>,
    /// Issues that are duplicates of this one.
    pub duplicates: Vec<RelatedIssue>,
}

/// A Linear project's status. Stored and printed canonically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProjectStatus {
    Backlog,
    Planned,
    Started,
    Paused,
    Completed,
    Canceled,
}
impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Backlog => "Backlog",
            ProjectStatus::Planned => "Planned",
            ProjectStatus::Started => "Started",
            ProjectStatus::Paused => "Paused",
            ProjectStatus::Completed => "Completed",
            ProjectStatus::Canceled => "Canceled",
        }
    }
    /// Ignores ASCII case and spaces. "In Progress", Linear's name for
    /// Started in its UI, is Started.
    pub fn parse(s: &str) -> Option<Self> {
        let key = loose_key(s);
        match key.as_str() {
            "backlog" => Some(ProjectStatus::Backlog),
            "planned" => Some(ProjectStatus::Planned),
            "started" | "inprogress" => Some(ProjectStatus::Started),
            "paused" => Some(ProjectStatus::Paused),
            "completed" => Some(ProjectStatus::Completed),
            "canceled" => Some(ProjectStatus::Canceled),
            _ => None,
        }
    }
    pub fn all() -> &'static [ProjectStatus] {
        use ProjectStatus::*;
        &[Backlog, Planned, Started, Paused, Completed, Canceled]
    }
}
impl FromStr for ProjectStatus {
    type Err = InvalidValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ProjectStatus::parse(s).ok_or_else(|| {
            let expected: Vec<&str> = ProjectStatus::all().iter().map(|s| s.as_str()).collect();
            InvalidValue(format!(
                "invalid project status \"{s}\"; expected one of: {}",
                expected.join(", ")
            ))
        })
    }
}
impl std::fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A Linear project inside a workspace.
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: ProjectStatus,
    /// YYYY-MM-DD.
    pub target_date: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A milestone, always inside one project for good.
#[derive(Debug, Clone, Serialize)]
pub struct Milestone {
    pub id: String,
    pub name: String,
    /// The project's name.
    pub project: String,
    #[serde(skip)]
    pub project_id: String,
    pub description: String,
    /// YYYY-MM-DD.
    pub target_date: Option<String>,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// How many issues there are in each status, in Status order. A stored
/// status is read by the rule every read uses, so a hand-edited "done" is
/// Done; one that names no status is `unreadable`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ByStatus {
    #[serde(rename = "Backlog")]
    pub backlog: i64,
    #[serde(rename = "Todo")]
    pub todo: i64,
    #[serde(rename = "InProgress")]
    pub in_progress: i64,
    #[serde(rename = "InReview")]
    pub in_review: i64,
    #[serde(rename = "Done")]
    pub done: i64,
    #[serde(rename = "Canceled")]
    pub canceled: i64,
    #[serde(rename = "Duplicate")]
    pub duplicate: i64,
    pub unreadable: i64,
}
impl ByStatus {
    /// Adds `n` issues whose status reads as `status` (None: unreadable).
    pub fn add(&mut self, status: Option<Status>, n: i64) {
        let slot = match status {
            Some(Status::Backlog) => &mut self.backlog,
            Some(Status::Todo) => &mut self.todo,
            Some(Status::InProgress) => &mut self.in_progress,
            Some(Status::InReview) => &mut self.in_review,
            Some(Status::Done) => &mut self.done,
            Some(Status::Canceled) => &mut self.canceled,
            Some(Status::Duplicate) => &mut self.duplicate,
            None => &mut self.unreadable,
        };
        *slot += n;
    }
    /// Each count with its JSON name, in Status order, unreadable last.
    pub fn entries(&self) -> [(&'static str, i64); 8] {
        [
            ("Backlog", self.backlog),
            ("Todo", self.todo),
            ("InProgress", self.in_progress),
            ("InReview", self.in_review),
            ("Done", self.done),
            ("Canceled", self.canceled),
            ("Duplicate", self.duplicate),
            ("unreadable", self.unreadable),
        ]
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IssueCounts {
    pub total: i64,
    pub by_status: ByStatus,
}

/// A milestone as `project show` lists it.
#[derive(Debug, Clone, Serialize)]
pub struct MilestoneSummary {
    pub id: String,
    pub name: String,
    pub target_date: Option<String>,
    pub sort_order: i64,
    /// How many issues it has.
    pub issues: i64,
}

/// `project show`: the project, its issue counts, and its milestones by
/// sort_order then name.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    #[serde(flatten)]
    pub project: Project,
    pub issues: IssueCounts,
    pub milestones: Vec<MilestoneSummary>,
}

/// `milestone show`: the milestone and its issue counts.
#[derive(Debug, Clone, Serialize)]
pub struct MilestoneDetail {
    #[serde(flatten)]
    pub milestone: Milestone,
    pub issues: IssueCounts,
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

/// A cycle as an issue names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleRef {
    pub id: String,
    pub name: String,
}

/// A cycle and how many issues are in it, as `cycle list` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct CycleListing {
    #[serde(flatten)]
    pub cycle: Cycle,
    pub issues: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMeta {
    pub name: String,
    pub key: String,
    pub description: String,
    pub created_at: i64,
}

pub fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Status::*;

    /// What a move does to one timestamp column.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Fx {
        /// unchanged
        K,
        /// set to `at` if unset, else unchanged
        S,
        /// cleared
        C,
    }
    use Fx::*;

    /// Every (from, to) pair, written out by hand from the rules rather than
    /// derived from them. `None` as `from` is create.
    /// Columns: started_at, completed_at, canceled_at.
    #[rustfmt::skip]
    const TABLE: &[(Option<Status>, Status, Fx, Fx, Fx)] = &[
        (None, Backlog, C, K, K), (None, Todo, C, K, K), (None, InProgress, S, K, K), (None, InReview, S, K, K),
        (None, Done, K, S, K), (None, Canceled, K, K, S), (None, Duplicate, K, K, S),

        (Some(Backlog), Backlog, K, K, K), (Some(Backlog), Todo, C, K, K), (Some(Backlog), InProgress, S, K, K),
        (Some(Backlog), InReview, S, K, K), (Some(Backlog), Done, K, S, K), (Some(Backlog), Canceled, K, K, S),
        (Some(Backlog), Duplicate, K, K, S),

        (Some(Todo), Backlog, C, K, K), (Some(Todo), Todo, K, K, K), (Some(Todo), InProgress, S, K, K),
        (Some(Todo), InReview, S, K, K), (Some(Todo), Done, K, S, K), (Some(Todo), Canceled, K, K, S),
        (Some(Todo), Duplicate, K, K, S),

        (Some(InProgress), Backlog, C, K, K), (Some(InProgress), Todo, C, K, K), (Some(InProgress), InProgress, K, K, K),
        (Some(InProgress), InReview, S, K, K), (Some(InProgress), Done, K, S, K), (Some(InProgress), Canceled, K, K, S),
        (Some(InProgress), Duplicate, K, K, S),

        (Some(InReview), Backlog, C, K, K), (Some(InReview), Todo, C, K, K), (Some(InReview), InProgress, S, K, K),
        (Some(InReview), InReview, K, K, K), (Some(InReview), Done, K, S, K), (Some(InReview), Canceled, K, K, S),
        (Some(InReview), Duplicate, K, K, S),

        (Some(Done), Backlog, C, C, K), (Some(Done), Todo, C, C, K), (Some(Done), InProgress, S, C, K),
        (Some(Done), InReview, S, C, K), (Some(Done), Done, K, K, K), (Some(Done), Canceled, K, C, S),
        (Some(Done), Duplicate, K, C, S),

        (Some(Canceled), Backlog, C, K, C), (Some(Canceled), Todo, C, K, C), (Some(Canceled), InProgress, S, K, C),
        (Some(Canceled), InReview, S, K, C), (Some(Canceled), Done, K, S, C), (Some(Canceled), Canceled, K, K, K),
        (Some(Canceled), Duplicate, K, K, S),

        (Some(Duplicate), Backlog, C, K, C), (Some(Duplicate), Todo, C, K, C), (Some(Duplicate), InProgress, S, K, C),
        (Some(Duplicate), InReview, S, K, C), (Some(Duplicate), Done, K, S, C), (Some(Duplicate), Canceled, K, K, S),
        (Some(Duplicate), Duplicate, K, K, K),
    ];

    fn issue(
        status: Status,
        started: Option<i64>,
        completed: Option<i64>,
        canceled: Option<i64>,
    ) -> Issue {
        Issue {
            id: IssueId::from_stored("id"),
            number: 1,
            identifier: String::new(),
            title: "t".into(),
            description: String::new(),
            status,
            priority: Priority::None,
            assignee: None,
            parent_id: None,
            parent_identifier: None,
            project_id: None,
            project: None,
            milestone_id: None,
            milestone: None,
            r#ref: None,
            creator: None,
            estimate: None,
            due_date: None,
            labels: vec![],
            cycles: vec![],
            created_at: 1,
            updated_at: 1,
            completed_at: completed,
            started_at: started,
            canceled_at: canceled,
        }
    }

    fn expect(fx: Fx, before: Option<i64>, at: i64) -> Option<i64> {
        match fx {
            K => before,
            S => before.or(Some(at)),
            C => None,
        }
    }

    #[test]
    fn the_table_covers_every_pair_once() {
        let mut seen = Vec::new();
        for &(from, to, ..) in TABLE {
            assert!(
                !seen.contains(&(from, to)),
                "duplicate row {from:?} -> {to:?}"
            );
            seen.push((from, to));
        }
        let all = Status::all();
        assert_eq!(all.len(), 7);
        for &to in all {
            assert!(seen.contains(&(None, to)), "missing create -> {to:?}");
            for &from in all {
                assert!(
                    seen.contains(&(Some(from), to)),
                    "missing {from:?} -> {to:?}"
                );
            }
        }
        assert_eq!(seen.len(), 7 + 7 * 7);
    }

    #[test]
    fn issue_ref_forms() {
        let num = |key: Option<&str>, n: i64| IssueRef::Number {
            key: key.map(String::from),
            number: n,
        };
        let u = "0f8fad5b-d9cb-469f-a165-70867728950e";
        for (s, want) in [
            ("7", num(None, 7)),
            ("#7", num(None, 7)),
            ("DREA-7", num(Some("DREA"), 7)),
            ("drea-7", num(Some("drea"), 7)),
            ("MY-KEY-7", num(Some("MY-KEY"), 7)),
            ("007", num(None, 7)),
            (u, IssueRef::Id(u.into())),
            (
                "0F8FAD5B-D9CB-469F-A165-70867728950E",
                IssueRef::Id(u.into()),
            ),
        ] {
            assert_eq!(s.parse::<IssueRef>(), Ok(want), "{s:?}");
        }
        for bad in [
            "",
            "0",
            "#0",
            "-3",
            "#-3",
            "+3",
            "#",
            "#x",
            "abc",
            "DREA-",
            "DREA-x",
            "DREA-0",
            "-7",
            "DREA 7-1",
            " 7",
            "7 ",
            "1.5",
            "99999999999999999999",
            "0f8fad5bd9cb469fa16570867728950e",
            "{0f8fad5b-d9cb-469f-a165-70867728950e}",
        ] {
            assert!(
                bad.parse::<IssueRef>().is_err(),
                "{bad:?} parsed as {:?}",
                bad.parse::<IssueRef>()
            );
        }
    }

    #[test]
    fn apply_lifecycle_matches_the_table() {
        const AT: i64 = 5_000;
        // Every timestamp unset, then every one planted with a sentinel that
        // "set if unset" must keep.
        let starts = [(None, None, None), (Some(111), Some(222), Some(333))];
        for &(from, to, fs, fc, fx) in TABLE {
            for (st, co, ca) in starts {
                let mut i = issue(to, st, co, ca);
                i.apply_lifecycle(from, AT);
                let got = (i.started_at, i.completed_at, i.canceled_at);
                let want = (expect(fs, st, AT), expect(fc, co, AT), expect(fx, ca, AT));
                assert_eq!(
                    got,
                    want,
                    "{from:?} -> {to:?} starting from {:?}",
                    (st, co, ca)
                );
                assert_eq!(i.status, to, "apply_lifecycle must not change the status");
                assert_eq!(
                    (i.created_at, i.updated_at),
                    (1, 1),
                    "apply_lifecycle must not touch other timestamps"
                );
            }
        }
    }
}
