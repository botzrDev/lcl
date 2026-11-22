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
    /// `KEY-N` with a key that isn't this workspace's. `holder`: an issue
    /// whose ref it is (its `KEY-N`, and the ref as stored), named as a
    /// hint; the identifier never resolves through it.
    #[error("{given} is not in this workspace (key {key}){}", holder.as_ref().map(|(issue, r)| format!("; {issue} has ref {r}")).unwrap_or_default())]
    WrongWorkspace {
        given: String,
        key: String,
        holder: Option<(String, String)>,
    },
    /// A ref another issue has, ignoring case. `holder` is its `KEY-N`.
    #[error("ref {given:?} is already on {holder}")]
    RefTaken { given: String, holder: String },
    /// A ref that lcl would read as a reference to one of this workspace's
    /// issues (`N`, `#N`, its `KEY-N`, a UUID): text must never have two
    /// meanings.
    #[error("ref {0:?} looks like one of this workspace's own issue references")]
    RefLooksOwn(String),
    #[error("{0} can't be its own parent")]
    SelfParent(String),
    /// `path` is the loop the new link would close, from the issue back to it.
    #[error("making {parent} the parent of {issue} would make a cycle: {path}")]
    ParentCycle {
        issue: String,
        parent: String,
        path: String,
    },
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
    /// The highest issue number there is has been handed out.
    #[error("this workspace has run out of issue numbers")]
    NumbersExhausted,
    /// A workspace file of no bytes. lcl never leaves one.
    #[error(
        "{0} is an empty file, not an lcl workspace; delete it, or run lcl init there to replace it"
    )]
    EmptyWorkspaceFile(String),
    /// `lcl tui` with stdin or stdout not a terminal.
    #[error("lcl tui needs a terminal: stdin and stdout must both be one")]
    NoTerminal,
    #[error("doc {0} can't be its own parent")]
    DocSelfParent(String),
    /// `path` runs from the doc up through its would-be parent back to it.
    #[error("making doc {parent} the parent of doc {doc} would make a cycle: {path}")]
    DocParentCycle {
        doc: String,
        parent: String,
        path: String,
    },
    /// A problem in an import file, which names its line and field (or the
    /// record, for a problem with the record as a whole).
    #[error("line {line}: {what}: {msg}")]
    ImportLine {
        line: usize,
        what: String,
        msg: String,
    },
    /// `has` lists what is there, as "1 issue and 2 labels".
    #[error(
        "workspace {workspace:?} is not empty: it has {has}; import only into an empty workspace (make one with lcl init or lcl workspace create)"
    )]
    ImportNotEmpty { workspace: String, has: String },
    #[error(
        "the file is for key {file} but this workspace's key is {workspace}; import it into a workspace with key {file}"
    )]
    ImportKeyMismatch { file: String, workspace: String },
    /// Everything that keeps the workspace from exporting as a file that
    /// imports, one problem per entry.
    #[error("can't export: {}:\n{}", counted(&[(problems.len() as i64, "problem")]), problems.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"))]
    ExportBlocked { problems: Vec<String> },
    #[error("{0} already exists; pass --force to replace it")]
    OutputExists(String),
    /// `lcl backup PATH` with something at PATH. It is never replaced.
    #[error("{0} already exists; a backup never replaces a file")]
    BackupExists(String),
    /// A copy of a database that could not be written. Nothing is left at
    /// `path`.
    #[error("can't write {path}: {reason}")]
    BackupFailed { path: String, reason: String },
    /// The copy taken before an upgrade could not be written, so the
    /// upgrade did not run.
    #[error(
        "can't upgrade {file}: the copy of it as it is, {snapshot}, could not be written ({reason}); nothing was changed"
    )]
    SnapshotFailed {
        file: String,
        snapshot: String,
        reason: String,
    },
    /// A command named an issue that is in the trash. Holds its `KEY-N`.
    #[error("{0} is in the trash; restore it with: lcl issue restore {0}")]
    InTrash(String),
    /// `issue restore` of an issue that is not in the trash.
    #[error("{0} is not in the trash")]
    NotInTrash(String),
    /// `issue restore` of an issue whose parent is in the trash.
    #[error(
        "{issue} can't be restored while its parent {parent} is in the trash; restore {parent} first"
    )]
    ParentInTrash { issue: String, parent: String },
    /// A command named a doc that is in the trash. Holds its id.
    #[error("doc {0} is in the trash; restore it with: lcl doc restore {0}")]
    DocInTrash(String),
    /// A command named a comment that is in the trash. Holds its id.
    #[error("comment {0} is in the trash; restore it with: lcl comment restore {0}")]
    CommentInTrash(String),
    /// A command named a label that is in the trash; its name stays taken.
    #[error("label {0:?} is in the trash; restore it with: lcl label restore {word}", word = shell_word(.0))]
    LabelInTrash(String),
    /// `doc restore` of a doc whose parent is in the trash.
    #[error(
        "doc {doc} can't be restored while its parent {parent} is in the trash; restore {parent} first"
    )]
    DocParentInTrash { doc: String, parent: String },
    /// A purge of a live doc, comment or label: only the trash is purged.
    /// `what` names it, as `doc ID`; `delete` is the command that moves it
    /// to the trash.
    #[error(
        "{what} is not in the trash; only what is in the trash can be purged ({delete} moves it there)"
    )]
    PurgeLiveItem { what: String, delete: String },
    /// `doc purge` of a doc with docs under it, in the trash with it.
    #[error(
        "doc {doc} has docs under it in the trash: {children}; purge them first, or empty the trash with lcl doc purge --all"
    )]
    DocPurgeHasChildren { doc: String, children: String },
    /// `issue purge` of a live issue: only the trash is purged.
    #[error(
        "{0} is not in the trash; only an issue in the trash can be purged (lcl issue delete {0} moves it there)"
    )]
    PurgeLive(String),
    /// `issue purge` of an issue whose sub-issues are in the trash with it.
    #[error(
        "{issue} has sub-issues {children} in the trash; purge them first, or empty the trash with lcl issue purge --all"
    )]
    PurgeHasChildren { issue: String, children: String },
    #[error("{0} can't be related to itself")]
    SelfRelation(String),
    /// `path` is the cycle the new row would close, as `A blocks B blocks A`.
    #[error("{blocker} blocking {blocked} would make a cycle: {path}")]
    BlocksCycle {
        blocker: String,
        blocked: String,
        path: String,
    },
    /// A second duplicate-of target. `current` is the one it has.
    /// `trashed`: the issue it is a duplicate of is in the trash, where no
    /// command but restore and purge can name it.
    #[error("{issue} is already a duplicate of {current}{}", if *trashed { format!(", which is in the trash; restore it (lcl issue restore {current}) and unrelate it, or purge it (lcl issue purge {current})") } else { format!("; unrelate it first: lcl issue unrelate {issue} duplicate-of {current}") })]
    AlreadyDuplicate {
        issue: String,
        current: String,
        trashed: bool,
    },
    /// `path` runs from the issue through each duplicate-of target back to it.
    #[error("making {issue} a duplicate of {target} would make a cycle: {path}")]
    DuplicateCycle {
        issue: String,
        target: String,
        path: String,
    },
    /// `unrelate` of a relation that isn't there, as typed.
    #[error("there is no relation {0}")]
    RelationNotFound(String),
    /// The workspace index is from a newer lcl.
    #[error(
        "the workspace index {path} is v{found}, newer than this lcl supports (v{supported}); upgrade lcl"
    )]
    RegistryTooNew {
        path: String,
        found: i64,
        supported: i64,
    },
    /// `workspace create` of a name that is registered, exactly.
    #[error("workspace {0:?} already exists")]
    WorkspaceExists(String),
    /// `workspace create` of a name whose slug another workspace has.
    #[error(
        "workspace {name:?} would use the same file ({slug}.sqlite) as the existing workspace {existing:?}; choose another name"
    )]
    SlugTaken {
        name: String,
        existing: String,
        slug: String,
    },
    /// A workspace file that is there although nothing registered it.
    #[error("a workspace file already exists at {0}")]
    WorkspaceFileExists(String),
    /// `init` with no name, in a directory that has none to lend it (/).
    #[error("this directory has no name for the workspace; give it one: lcl init NAME --key KEY")]
    InitNeedsName,
    /// `init` where a workspace with another name or key already is.
    #[error(
        "this directory already has workspace {name:?} (key {key}) at {path}; init doesn't rename or re-key a workspace"
    )]
    InitMismatch {
        name: String,
        key: String,
        path: String,
    },
    /// `init` of a name whose slug another workspace has.
    #[error("the name {name:?} is taken: workspace {existing:?} is registered at {path}")]
    NameTaken {
        name: String,
        existing: String,
        path: String,
    },
    /// `init` of a name registered, exactly, to another place.
    #[error("workspace {name:?} is already registered at {path}")]
    AlreadyRegistered { name: String, path: String },
    /// A workspace name with no letters or digits, so no file name.
    #[error("workspace name {0:?} has no letters or digits")]
    NoSlug(String),
    #[error("workspace {name:?} not found: no file at {path}; see lcl workspace list")]
    WorkspaceNotFound { name: String, path: String },
    #[error("no workspace found; run lcl init or pass --workspace")]
    NoWorkspace,
    /// No home directory, where lcl needs one: see `registry::home_dir`.
    #[error(
        "neither HOME nor USERPROFILE gives a home directory; lcl keeps its workspace registry in $HOME/.lcl, or on Windows, when HOME is not set, in %USERPROFILE%\\.lcl"
    )]
    NoHome,
    /// A --project, or a project argument, naming no project. `workspace`:
    /// the name is a registered workspace's, so whoever typed it may mean
    /// lcl 0.1.0's --project (the CLI sets this; the store never does).
    #[error("project {name:?} not found in this workspace{}", if *workspace { format!("; {name:?} is a workspace: the workspace flag is now --workspace (-w)") } else { String::new() })]
    ProjectNotFound { name: String, workspace: bool },
    /// A project name taken, ignoring case. `existing` is its spelling.
    #[error("project {name:?} already exists{}", if name == existing { String::new() } else { format!(" (as {existing:?})") })]
    ProjectExists { name: String, existing: String },
    /// `project delete` of a project that issues or milestones reference.
    /// `issues` counts every issue in it, `trashed` those of them in the
    /// trash, which hold their place for `issue restore`.
    #[error("project {name:?} has {}; move or delete {} first{}", counted(&[(*issues, "issue"), (*milestones, "milestone")]), them(issues + milestones), in_trash(*trashed))]
    ProjectInUse {
        name: String,
        issues: i64,
        milestones: i64,
        trashed: i64,
    },
    /// No milestone with this name or id (`project`: where it was looked for).
    #[error("milestone {name:?} not found{}", project.as_ref().map(|p| format!(" in project {p:?}")).unwrap_or_default())]
    MilestoneNotFound {
        name: String,
        project: Option<String>,
    },
    /// An issue's milestone that exists, but only in other projects.
    #[error(
        "milestone {name:?} is not in project {project:?} (it is in {})",
        projects_named(elsewhere)
    )]
    MilestoneElsewhere {
        name: String,
        project: String,
        elsewhere: Vec<String>,
    },
    /// A milestone name that several projects have, given without --project.
    #[error(
        "milestone {name:?} is in {}; pass --project to choose",
        projects_named(projects)
    )]
    MilestoneAmbiguous { name: String, projects: Vec<String> },
    /// A milestone name taken in its project, ignoring case.
    #[error("milestone {name:?} already exists in project {project:?}{}", if name == existing { String::new() } else { format!(" (as {existing:?})") })]
    MilestoneExists {
        name: String,
        existing: String,
        project: String,
    },
    /// `milestone delete` of a milestone that issues reference.
    #[error("milestone {name:?} has {}; move or delete {} first{}", counted(&[(*issues, "issue")]), them(*issues), in_trash(*trashed))]
    MilestoneInUse {
        name: String,
        issues: i64,
        trashed: i64,
    },
    /// A milestone for an issue that would have no project.
    #[error("milestone {0:?} needs a project: the issue has none; pass --project too")]
    MilestoneWithoutProject(String),
    /// `issue list --project none --milestone M`: nothing can match.
    #[error(
        "--milestone {0:?} can't be used with --project none: issues in no project have no milestone"
    )]
    MilestoneInNoProject(String),
    /// A comment id (matched ignoring case) that names no comment.
    #[error("comment {0} not found")]
    CommentNotFound(String),
    /// A cycle name taken, comparing both trimmed and ignoring case.
    /// `existing` is its stored spelling.
    #[error("cycle {name:?} already exists{}", if name == existing { String::new() } else { format!(" (as {existing:?})") })]
    CycleExists { name: String, existing: String },
    /// A cycle name several cycles have. `ids` are theirs, oldest first.
    #[error("cycle {name:?} names {} cycles: {}; give one by its id", ids.len(), ids.join(", "))]
    CycleAmbiguous { name: String, ids: Vec<String> },
    /// No cycle with this name or id.
    #[error("cycle {0:?} not found")]
    CycleNotFound(String),
    /// `cycle delete` of a cycle that issues are in.
    #[error("cycle {name:?} has {}; move {} to another cycle or none first{}", counted(&[(*issues, "issue")]), them(*issues), in_trash(*trashed))]
    CycleInUse {
        name: String,
        issues: i64,
        trashed: i64,
    },
    /// `cycle update` leaving the end before the start, one of them the
    /// stored date. (Both typed is a usage error, as on create.)
    #[error("cycle {name:?} would end {end}, before its start {start}")]
    CycleEndBeforeStart {
        name: String,
        start: String,
        end: String,
    },
    #[error("the search query is empty")]
    EmptyQuery,
    /// Every word lacks token characters (`--`, `*`, a lone `"`).
    #[error("the search query {0:?} has no words to search for")]
    NothingToSearch(String),
    /// A doc delete while other docs name it as parent.
    #[error("doc {doc} has sub-docs {children}; reparent or delete them first")]
    DocHasChildren { doc: String, children: String },
    /// The search index does not match the rows it indexes. External-content
    /// FTS is keyed by rowid, which `VACUUM` can renumber.
    #[error("the search index {table} does not match its rows: {detail}")]
    FtsMismatch { table: &'static str, detail: String },
    /// `PRAGMA foreign_key_check` after a migration.
    #[error("foreign keys do not hold after migration: {0}")]
    ForeignKeyCheck(String),
    #[error("count(*) returned {0}")]
    CountOverflow(i64),
    #[error("meta {key} is not a number: {value:?}")]
    BadMetaNumber { key: &'static str, value: String },
    #[error("project status {value:?} is unknown")]
    UnknownProjectStatus { value: String },
    #[error("{what} {id} vanished inside its own transaction")]
    Vanished { what: &'static str, id: String },
    #[error("comment {id}: changed {changed} rows, not 1")]
    CommentWriteCount { id: String, changed: usize },
    #[error("can't encode ids: {0}")]
    Encode(String),
    #[error("database schema version {0} is invalid")]
    BadSchemaVersion(i64),
    #[error("the workspace index has a workspace of unknown kind {0:?}")]
    UnknownWorkspaceKind(String),
    #[error("{path} has no directory")]
    NoParentDir { path: String },
    #[error("{path} was left open after building the workspace")]
    WorkspaceLeftOpen { path: String },
    #[error("not UTF-8")]
    PathNotUtf8,
    #[error("the workspace index's schema version {0:?} is invalid")]
    BadRegistryVersion(String),
    #[error("the workspace index's schema version {0} is invalid")]
    BadRegistryVersionNumber(i64),
    #[error("lcl made an export that it would refuse to import ({0}); this is a bug in lcl")]
    ExportRoundTrip(String),
}
pub type Result<T> = std::result::Result<T, LclError>;

/// What kind of error it is, for a program reading `--json` errors. Every
/// usage error (exit 2) is `Usage`; the rest exit 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The command line is wrong, whatever is stored: exit 2.
    Usage,
    /// Something named isn't there.
    NotFound,
    /// The request conflicts with what is stored.
    Conflict,
    /// A file from a newer lcl, or values this lcl doesn't know.
    Schema,
    Io,
    Other,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Usage => "usage",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Schema => "schema",
            ErrorKind::Io => "io",
            ErrorKind::Other => "other",
        }
    }
}

impl LclError {
    /// Its kind. The match has no catch-all, so a new variant must be given
    /// one. A usage error (exit 2) is one the command can't escape in any
    /// state of the open workspace. One that other stored data would avoid
    /// (what a reference resolves to; the labels, milestones, relations or
    /// parents there) exits 1, even when what was typed looks the same.
    pub fn kind(&self) -> ErrorKind {
        use ErrorKind::*;
        match self {
            LclError::Db(e) => db_kind(e),
            LclError::NoHome
            | LclError::CountOverflow(_)
            | LclError::BadMetaNumber { .. }
            | LclError::UnknownProjectStatus { .. }
            | LclError::Vanished { .. }
            | LclError::CommentWriteCount { .. }
            | LclError::Encode(_)
            | LclError::UnknownWorkspaceKind(_)
            | LclError::NoParentDir { .. }
            | LclError::WorkspaceLeftOpen { .. }
            | LclError::PathNotUtf8
            | LclError::BadRegistryVersion(_)
            | LclError::BadRegistryVersionNumber(_)
            | LclError::ExportRoundTrip(_) => Other,
            LclError::Io(_) | LclError::BackupFailed { .. } | LclError::SnapshotFailed { .. } => Io,
            LclError::SchemaTooNew { .. }
            | LclError::UnknownStatus { .. }
            | LclError::UnknownPriority { .. }
            | LclError::RegistryTooNew { .. }
            | LclError::FtsMismatch { .. }
            | LclError::ForeignKeyCheck(_)
            | LclError::BadSchemaVersion(_) => Schema,
            LclError::NotFound(_)
            | LclError::IssueNotFound(_)
            | LclError::LabelNotFound(_)
            | LclError::DocNotFound(_)
            | LclError::RelationNotFound(_)
            | LclError::WorkspaceNotFound { .. }
            | LclError::NoWorkspace
            | LclError::ProjectNotFound { .. }
            | LclError::MilestoneNotFound { .. }
            | LclError::UnknownLabel(_)
            | LclError::CommentNotFound(_)
            | LclError::CycleNotFound(_) => NotFound,
            LclError::RefTaken { .. }
            | LclError::SelfParent(_)
            | LclError::ParentCycle { .. }
            | LclError::HasChildren { .. }
            | LclError::LabelExists { .. }
            | LclError::DocSelfParent(_)
            | LclError::DocParentCycle { .. }
            | LclError::ImportNotEmpty { .. }
            | LclError::NumbersExhausted
            | LclError::EmptyWorkspaceFile(_)
            | LclError::NoTerminal
            | LclError::ImportKeyMismatch { .. }
            | LclError::ExportBlocked { .. }
            | LclError::OutputExists(_)
            | LclError::BackupExists(_)
            | LclError::InTrash(_)
            | LclError::NotInTrash(_)
            | LclError::ParentInTrash { .. }
            | LclError::PurgeLive(_)
            | LclError::DocInTrash(_)
            | LclError::CommentInTrash(_)
            | LclError::LabelInTrash(_)
            | LclError::DocParentInTrash { .. }
            | LclError::PurgeLiveItem { .. }
            | LclError::DocPurgeHasChildren { .. }
            | LclError::PurgeHasChildren { .. }
            | LclError::SelfRelation(_)
            | LclError::BlocksCycle { .. }
            | LclError::AlreadyDuplicate { .. }
            | LclError::DuplicateCycle { .. }
            | LclError::WorkspaceExists(_)
            | LclError::SlugTaken { .. }
            | LclError::WorkspaceFileExists(_)
            | LclError::InitMismatch { .. }
            | LclError::NameTaken { .. }
            | LclError::AlreadyRegistered { .. }
            | LclError::ProjectExists { .. }
            | LclError::ProjectInUse { .. }
            | LclError::MilestoneExists { .. }
            | LclError::MilestoneInUse { .. }
            | LclError::MilestoneElsewhere { .. }
            | LclError::MilestoneAmbiguous { .. }
            | LclError::MilestoneWithoutProject(_)
            | LclError::CycleExists { .. }
            | LclError::CycleAmbiguous { .. }
            | LclError::CycleInUse { .. }
            | LclError::CycleEndBeforeStart { .. }
            | LclError::DocHasChildren { .. } => Conflict,
            LclError::WrongWorkspace { .. }
            | LclError::RefLooksOwn(_)
            | LclError::ImportLine { .. }
            | LclError::InitNeedsName
            | LclError::NoSlug(_)
            | LclError::MilestoneInNoProject(_)
            | LclError::EmptyQuery
            | LclError::NothingToSearch(_) => Usage,
        }
    }

    /// The request itself is wrong, whatever the data: a CLI reports these
    /// as usage errors (exit 2). Everything else is about the state of the
    /// data or the system.
    pub fn is_usage(&self) -> bool {
        self.kind() == ErrorKind::Usage
    }
}

/// A SQLite error about the file or the disk (it can't be opened, read,
/// written, or grown) is io; any other is other.
fn db_kind(e: &rusqlite::Error) -> ErrorKind {
    use rusqlite::ErrorCode::{CannotOpen, DiskFull, ReadOnly, SystemIoFailure};
    match e.sqlite_error_code() {
        Some(CannotOpen | SystemIoFailure | DiskFull | ReadOnly) => ErrorKind::Io,
        _ => ErrorKind::Other,
    }
}

/// The steps of a loop, joined by `sep`. A long one keeps its first and last
/// steps and says how many it left out, so the message stays readable.
pub(crate) fn chain(steps: &[String], sep: &str) -> String {
    const ENDS: usize = 5;
    if steps.len() <= 2 * ENDS + 1 {
        return steps.join(sep);
    }
    let left_out = steps.len() - 2 * ENDS;
    format!(
        "{}{sep}… {left_out} more …{sep}{}",
        steps[..ENDS].join(sep),
        steps[steps.len() - ENDS..].join(sep)
    )
}

/// "3 issues and 1 milestone": each non-zero count with its noun.
fn counted(parts: &[(i64, &str)]) -> String {
    let parts: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n != 0)
        .map(|(n, noun)| format!("{n} {noun}{}", if *n == 1 { "" } else { "s" }))
        .collect();
    parts.join(" and ")
}

/// "it" for one thing, "them" for more.
/// The end of an in-use refusal when `trashed` of the issues counted are in
/// the trash: they can't be moved until they are restored.
fn in_trash(trashed: i64) -> String {
    match trashed {
        0 => String::new(),
        1 => " (1 is in the trash: lcl issue list --trashed shows it; restore it to move it, or purge it)".into(),
        n => format!(" ({n} are in the trash: lcl issue list --trashed shows them; restore them to move them, or purge them)"),
    }
}

fn them(n: i64) -> &'static str {
    if n == 1 { "it" } else { "them" }
}

/// `project "Q"`, or `projects "Q" and "R"`, or `projects "Q", "R" and "S"`.
fn projects_named(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    match quoted.as_slice() {
        [one] => format!("project {one}"),
        [rest @ .., last] => format!("projects {} and {last}", rest.join(", ")),
        [] => "no project".to_string(),
    }
}

/// `s` as one shell word: as is when that's safe, else single-quoted.
fn shell_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/:@%+=,-".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
