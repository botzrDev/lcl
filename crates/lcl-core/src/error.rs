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
    #[error("{0} can't be related to itself")]
    SelfRelation(String),
    /// `path` is the cycle the new row would close, as `A blocks B blocks A`.
    #[error("{blocker} blocking {blocked} would make a cycle: {path}")]
    BlocksCycle { blocker: String, blocked: String, path: String },
    /// A second duplicate-of target. `current` is the one it has.
    #[error("{issue} is already a duplicate of {current}; unrelate it first: lcl issue unrelate {issue} duplicate-of {current}")]
    AlreadyDuplicate { issue: String, current: String },
    /// `path` runs from the issue through each duplicate-of target back to it.
    #[error("making {issue} a duplicate of {target} would make a cycle: {path}")]
    DuplicateCycle { issue: String, target: String, path: String },
    /// `unrelate` of a relation that isn't there, as typed.
    #[error("there is no relation {0}")]
    RelationNotFound(String),
    /// The workspace index is from a newer lcl.
    #[error("the workspace index {path} is v{found}, newer than this lcl supports (v{supported}); upgrade lcl")]
    RegistryTooNew { path: String, found: i64, supported: i64 },
    /// `workspace create` of a name that is registered, exactly.
    #[error("workspace {0:?} already exists")]
    WorkspaceExists(String),
    /// `workspace create` of a name whose slug another workspace has.
    #[error("workspace {name:?} would use the same file ({slug}.sqlite) as the existing workspace {existing:?}; choose another name")]
    SlugTaken { name: String, existing: String, slug: String },
    /// A workspace file that is there although nothing registered it.
    #[error("a workspace file already exists at {0}")]
    WorkspaceFileExists(String),
    /// A workspace name with no letters or digits, so no file name.
    #[error("workspace name {0:?} has no letters or digits")]
    NoSlug(String),
    #[error("workspace {name:?} not found: no file at {path}; see lcl workspace list")]
    WorkspaceNotFound { name: String, path: String },
    #[error("no workspace found; run lcl init or pass --workspace")]
    NoWorkspace,
    /// HOME unset or empty, where lcl needs it.
    #[error("HOME is not set; lcl keeps its workspace registry in $HOME/.lcl")]
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
    #[error("project {name:?} has {}; move or delete {} first", counted(&[(*issues, "issue"), (*milestones, "milestone")]), them(issues + milestones))]
    ProjectInUse { name: String, issues: i64, milestones: i64 },
    /// No milestone with this name or id (`project`: where it was looked for).
    #[error("milestone {name:?} not found{}", project.as_ref().map(|p| format!(" in project {p:?}")).unwrap_or_default())]
    MilestoneNotFound { name: String, project: Option<String> },
    /// An issue's milestone that exists, but only in other projects.
    #[error("milestone {name:?} is not in project {project:?} (it is in {})", projects_named(elsewhere))]
    MilestoneElsewhere { name: String, project: String, elsewhere: Vec<String> },
    /// A milestone name that several projects have, given without --project.
    #[error("milestone {name:?} is in {}; pass --project to choose", projects_named(projects))]
    MilestoneAmbiguous { name: String, projects: Vec<String> },
    /// A milestone name taken in its project, ignoring case.
    #[error("milestone {name:?} already exists in project {project:?}{}", if name == existing { String::new() } else { format!(" (as {existing:?})") })]
    MilestoneExists { name: String, existing: String, project: String },
    /// `milestone delete` of a milestone that issues reference.
    #[error("milestone {name:?} has {}; move or delete {} first", counted(&[(*issues, "issue")]), them(*issues))]
    MilestoneInUse { name: String, issues: i64 },
    /// A milestone for an issue that would have no project.
    #[error("milestone {0:?} needs a project: the issue has none; pass --project too")]
    MilestoneWithoutProject(String),
    /// `issue list --project none --milestone M`: nothing can match.
    #[error("--milestone {0:?} can't be used with --project none: issues in no project have no milestone")]
    MilestoneInNoProject(String),
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
                | LclError::SelfRelation(_)
                | LclError::BlocksCycle { .. }
                | LclError::AlreadyDuplicate { .. }
                | LclError::DuplicateCycle { .. }
                | LclError::NoSlug(_)
                | LclError::MilestoneElsewhere { .. }
                | LclError::MilestoneAmbiguous { .. }
                | LclError::MilestoneWithoutProject(_)
                | LclError::MilestoneInNoProject(_)
                | LclError::EmptyQuery
                | LclError::NothingToSearch(_)
        )
    }
}

/// "3 issues and 1 milestone": each non-zero count with its noun.
fn counted(parts: &[(i64, &str)]) -> String {
    let parts: Vec<String> = parts.iter().filter(|(n, _)| *n != 0).map(|(n, noun)| format!("{n} {noun}{}", if *n == 1 { "" } else { "s" })).collect();
    parts.join(" and ")
}

/// "it" for one thing, "them" for more.
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
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._/:@%+=,-".contains(c));
    if plain { s.to_string() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}
