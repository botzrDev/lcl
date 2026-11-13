use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::parser::ValueSource;
use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
use lcl_core::db::{AssigneeFilter, Change, IssueFilter, IssueSort, MilestoneUpdate, Page, Placement, ProjectFilter, ProjectUpdate, Store};
use lcl_core::error::{ErrorKind, LclError};
use lcl_core::models::*;
use lcl_core::parse;
use lcl_core::registry::{self, Registry, WorkspaceKey};
use lcl_core::search::SearchQuery;
use std::io::{IsTerminal, Read, Write};
use std::time::Duration;
use std::path::PathBuf;
use std::process::ExitCode;

/// `println!` that returns the error when stdout fails (`?`), as
/// StdoutClosed when its reader went away, instead of panicking.
macro_rules! outln {
    ($($arg:tt)*) => {
        writeln!(std::io::stdout(), $($arg)*).map_err(|e| stdout_failed(e, "can't write to stdout"))?
    };
}

#[derive(Parser)]
#[command(
    name = "lcl",
    version,
    about = "Local Linear-like tracker (one SQLite database per workspace)",
    after_help = "Exit status: 0 success; 1 the command failed (not found, a conflict with what is stored, a newer schema, an I/O error); 2 the command line is wrong (a bad flag, value or combination).\nWith --json, an error is one line on stderr: {\"error\":{\"kind\":\"usage|not_found|conflict|schema|io|other\",\"message\":\"...\"}}"
)]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// The registered workspace NAME: a global one
    /// (~/.lcl/projects/<slug>.sqlite) or one lcl init made. Without it, the
    /// nearest .lcl/data.sqlite at or above the current directory.
    #[arg(short = 'w', long, global = true, value_name = "NAME", env = "LCL_WORKSPACE")]
    workspace: Option<String>,
    /// The old name of --workspace: refused with a hint. Not global, so only
    /// before the subcommand; a subcommand may have its own --project.
    #[arg(long, hide = true, value_name = "NAME")]
    project: Option<String>,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Make a workspace in the current directory, at .lcl/data.sqlite, and
    /// register it, so --workspace NAME finds it from anywhere.
    ///
    /// Run again with the same name and key, it does nothing. It never
    /// renames or re-keys a workspace, and a name another workspace has
    /// (compared by slug, as workspace create compares them) is refused.
    Init {
        /// Its name; the current directory's name if left out.
        #[arg(value_parser = parse_workspace_name)] name: Option<String>,
        /// The prefix of its issue identifiers, as DRM in DRM-12: a letter,
        /// then up to 9 letters or digits. Uppercased.
        #[arg(long, value_name = "KEY")] key: WorkspaceKey,
    },
    /// Create, list and show workspaces.
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Projects in this workspace: create, list, show, update, delete.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Milestones, each in one project for good: create, list, show, update, delete.
    Milestone {
        #[command(subcommand)]
        cmd: MilestoneCmd,
    },
    /// Issues in this workspace: create, list, show, update, delete, relate, comment.
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    /// Docs in this workspace: create, list, show, update, delete.
    Doc {
        #[command(subcommand)]
        cmd: DocCmd,
    },
    /// Labels in this workspace: create, list, delete.
    Label {
        #[command(subcommand)]
        cmd: LabelCmd,
    },
    /// Cycles in this workspace: create, list.
    Cycle {
        #[command(subcommand)]
        cmd: CycleCmd,
    },
    /// Search issues (with their comments) and docs.
    ///
    /// The query is split on whitespace, and every word must match. Words are
    /// literal: AND, OR, NOT, NEAR, quotes, parentheses and punctuation are
    /// plain text, and BZR-1532, v1.3.1 or writer_lock.rs match as written. A
    /// word ending in * matches as a prefix: writ* finds writer. Case and
    /// diacritics are ignored: cafe finds café.
    ///
    /// An issue matches when every word is in its title and description
    /// together, or every word is in one of its comments. Words split between
    /// the issue and a comment, or across two comments, don't match.
    ///
    /// A query that is exactly an issue's KEY-N or #N lists that issue first.
    ///
    /// Quoting the query is optional. Words that start with - go after --,
    /// e.g. lcl search -- --no-commit.
    Search {
        /// The words to search for.
        #[arg(value_name = "WORD", required = true, num_args = 1..)]
        words: Vec<String>,
        /// Show at most N issues and N docs.
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
    },
    /// Write the whole workspace as a text file (format lcl-export/1).
    ///
    /// One JSON record per line: the workspace, then labels, projects,
    /// milestones, cycles, issues, comments, relations and docs. Issues keep
    /// their numbers; nothing in the file depends on this workspace's ids.
    /// The same workspace always exports to the same bytes, and every file
    /// export writes, import reads back exactly.
    ///
    /// A stored value import would refuse (from a hand edit, or an older
    /// lcl) stops the export: each is listed, with the command that fixes
    /// it where there is one, and nothing is written.
    Export {
        /// Write to PATH (through a temporary file in its directory, renamed
        /// into place) instead of stdout. `-` is stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<String>,
        /// Replace PATH if it exists.
        #[arg(long, requires = "output")]
        force: bool,
    },
    /// Restore a file written by lcl export (or a converter) into this
    /// workspace, which must be empty and have the file's key.
    ///
    /// Numbers, refs, creators, comment authors and timestamps are kept as
    /// the file gives them; no status rules run. Every value must pass the
    /// rules the commands apply, and the file is checked whole before
    /// anything is written: a problem exits 2 naming its line and field,
    /// and the workspace is left as it was. The import is one transaction.
    Import {
        /// The file, or `-` for stdin.
        #[arg(value_name = "PATH", value_parser = read_import)]
        input: ImportInput,
        /// Check and count everything, then roll back: nothing is written.
        #[arg(long)]
        dry_run: bool,
    },
    /// Open this workspace in a terminal UI.
    Tui,
}

/// An import file's bytes, read as the arguments are parsed.
#[derive(Clone)]
struct ImportInput(Vec<u8>);

/// `import PATH`: the file, or stdin for `-`, as bytes (import reports a
/// line that isn't UTF-8 itself).
fn read_import(path: &str) -> std::result::Result<ImportInput, String> {
    if path == "-" {
        read_stdin("import").map(ImportInput)
    } else {
        std::fs::read(path).map(ImportInput).map_err(|e| format!("can't read {path}: {e}"))
    }
}

/// Writes an export to `path` through a temporary file in the same
/// directory, so a reader never sees half a file. Without `force` an
/// existing `path` is refused, also one that appears while writing.
fn write_export(path: &str, text: &str, force: bool) -> Result<()> {
    let target = std::path::Path::new(path);
    let exists = |p: &std::path::Path| std::fs::symlink_metadata(p).is_ok();
    if !force && exists(target) {
        return Err(LclError::OutputExists(path.to_string()).into());
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "export".into());
    let tmp = dir.join(format!(".{name}.lcl-export-{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    })();
    let placed = written.and_then(|()| {
        if force {
            return std::fs::rename(&tmp, target);
        }
        // A hard link fails if `path` exists, so no file is ever replaced.
        match std::fs::hard_link(&tmp, target) {
            Ok(()) => std::fs::remove_file(&tmp),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
            // A filesystem without hard links: check, then rename.
            Err(_) if exists(target) => Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)),
            Err(_) => std::fs::rename(&tmp, target),
        }
    });
    if let Err(e) = placed {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(LclError::OutputExists(path.to_string()).into());
        }
        return Err(anyhow::Error::new(e).context(format!("can't write {path}")));
    }
    Ok(())
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Create a global workspace, at ~/.lcl/projects/<slug>.sqlite.
    ///
    /// Its slug, which names the file, is NAME with ASCII letters lowercased
    /// and every character but letters and digits made -. Two names with the
    /// same slug can't both exist. For a workspace in the current directory,
    /// use lcl init.
    Create {
        /// Its name, trimmed: with a letter or digit, and a slug no other workspace has.
        #[arg(value_parser = parse_workspace_name)] name: String,
        /// The prefix of its issue identifiers, as DRM in DRM-12: a letter,
        /// then up to 9 letters or digits. Uppercased.
        #[arg(long, value_name = "KEY")] key: WorkspaceKey,
    },
    /// List registered workspaces: name, slug and path. One whose file is
    /// gone is marked missing.
    List,
    /// Show the current workspace: name, key, path, schema version and
    /// creation time.
    Show,
}

#[derive(Subcommand)]
enum IssueCmd {
    // Status, Priority and IssueRef parse through their FromStr impls, and
    // dates, estimates and colors through the parsers below. A bad value is a
    // clap error: exit 2, before any database is opened. Issue arguments allow
    // a leading '-' so that "-3" reaches the IssueRef parser and is rejected
    // there, rather than being taken for a flag.
    /// Create an issue. It gets the next number: a number is never handed out twice.
    Create {
        /// The issue's title.
        #[arg(long)] title: String,
        /// The description inline. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        /// Backlog, Todo, In Progress, In Review, Done, Canceled or Duplicate; case, spaces, _ and - are ignored.
        #[arg(long, default_value = "Backlog")] status: Status,
        /// 0-4, or none, urgent, high, medium or low (0 is none); case is ignored.
        #[arg(long, default_value = "0")] priority: Priority,
        /// Who it is assigned to: a name, trimmed, not blank.
        #[arg(long, value_parser = parse_assignee)] assignee: Option<String>,
        /// Its parent issue: KEY-N, #N, N or a UUID.
        #[arg(long, allow_negative_numbers = true)] parent: Option<IssueRef>,
        /// A label name (ignoring case); repeat for more.
        #[arg(long, value_parser = parse_label_name)] label: Vec<String>,
        /// Its estimate: a whole number, 0 or more.
        #[arg(long, allow_negative_numbers = true, value_parser = parse_estimate)] estimate: Option<i64>,
        /// Its due date, as YYYY-MM-DD.
        #[arg(long, value_parser = parse_date)] due: Option<NaiveDate>,
        /// The project to put it in, by name (ignoring case) or id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name, conflicts_with = "no_project")] project: Option<String>,
        /// Put it in no project (the default).
        #[arg(long, conflicts_with = "milestone")] no_project: bool,
        /// Its milestone, by name (ignoring case) or id: one in the --project project, which it needs.
        #[arg(long, value_name = "MILESTONE", value_parser = parse_milestone_name, conflicts_with = "no_milestone", requires = "project")] milestone: Option<String>,
        /// Give it no milestone (the default).
        #[arg(long)] no_milestone: bool,
        /// The id it has elsewhere, as BZR-1532 for a Linear issue: on no
        /// other issue (ignoring case), and not shaped like a reference to one
        /// of this workspace's issues (N, #N, its KEY-N, a UUID).
        #[arg(long = "ref", value_name = "REF", value_parser = parse_ref)] issue_ref: Option<String>,
        /// Who is creating it. Without it: LCL_AUTHOR, else USER.
        #[arg(long, value_name = "NAME", value_parser = parse_author)] author: Option<String>,
    },
    /// List issues: 50 at a time unless told otherwise, newest number first.
    ///
    /// Filters narrow the list together. A repeated --status matches any of
    /// them; a repeated --label requires all of them.
    List {
        /// Only issues with this status; repeat for any of several.
        #[arg(long, value_name = "STATUS")] status: Vec<Status>,
        /// Only issues assigned to NAME, ignoring ASCII case; `none` for unassigned issues.
        #[arg(long, value_name = "NAME", value_parser = parse_assignee_filter)] assignee: Option<AssigneeFilter>,
        /// Only issues with this label; repeat to require several.
        #[arg(long, value_name = "LABEL", value_parser = parse_label_name)] label: Vec<String>,
        /// Only the direct sub-issues of this issue.
        #[arg(long, allow_negative_numbers = true)] parent: Option<IssueRef>,
        /// Only issues with a blocker that isn't Done, Canceled or Duplicate.
        #[arg(long, conflicts_with = "unblocked")] blocked: bool,
        /// Only issues with no such blocker.
        #[arg(long)] unblocked: bool,
        /// Only issues in this project, by name (ignoring case) or id; `none`
        /// (any case) for issues in no project. A project named "none" is
        /// selected by its id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_filter)] project: Option<ProjectFilter>,
        /// Only issues with this milestone, by name (ignoring case) or id: in
        /// the --project project if given, else in any project.
        #[arg(long, value_name = "MILESTONE", value_parser = parse_milestone_name)] milestone: Option<String>,
        /// number, created or updated (each newest first), or priority (Urgent first, None last).
        #[arg(long, value_name = "ORDER", default_value = "number")] sort: IssueSort,
        /// Show at most N issues.
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))] limit: u32,
        /// Skip the first N issues.
        #[arg(long, value_name = "N", default_value_t = 0)] offset: u32,
        /// Show every matching issue.
        #[arg(long, conflicts_with_all = ["limit", "offset"])] all: bool,
    },
    /// Show an issue: its fields, relations, sub-issues and comments.
    Show {
        /// The issue: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)]
        id: IssueRef,
    },
    /// Change an issue: only what the flags name changes.
    Update {
        /// The issue: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)] id: IssueRef,
        /// Its new title.
        #[arg(long)] title: Option<String>,
        /// The description inline; "" clears it. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        /// Backlog, Todo, In Progress, In Review, Done, Canceled or Duplicate; case, spaces, _ and - are ignored.
        #[arg(long)] status: Option<Status>,
        /// 0-4, or none, urgent, high, medium or low (0 is none); case is ignored.
        #[arg(long)] priority: Option<Priority>,
        /// Assign it to this name: trimmed, not blank.
        #[arg(long, value_parser = parse_assignee, conflicts_with = "no_assignee")] assignee: Option<String>,
        /// Unassign the issue.
        #[arg(long)] no_assignee: bool,
        /// Its estimate: a whole number, 0 or more.
        #[arg(long, allow_negative_numbers = true, value_parser = parse_estimate, conflicts_with = "no_estimate")] estimate: Option<i64>,
        /// Clear the estimate.
        #[arg(long)] no_estimate: bool,
        /// Its due date, as YYYY-MM-DD.
        #[arg(long, value_parser = parse_date, conflicts_with = "no_due")] due: Option<NaiveDate>,
        /// Clear the due date.
        #[arg(long)] no_due: bool,
        /// Replace all labels with these (names, ignoring case); repeat for more.
        #[arg(long, value_parser = parse_label_name, conflicts_with_all = ["add_label", "remove_label", "clear_labels"])] label: Vec<String>,
        /// Add a label, keeping the others; repeatable.
        #[arg(long, value_parser = parse_label_name)] add_label: Vec<String>,
        /// Remove a label (a no-op if the issue doesn't have it); repeatable.
        #[arg(long, value_parser = parse_label_name)] remove_label: Vec<String>,
        /// Remove every label.
        #[arg(long, conflicts_with_all = ["add_label", "remove_label"])] clear_labels: bool,
        /// Move the issue under this one.
        #[arg(long, allow_negative_numbers = true, conflicts_with = "no_parent")] parent: Option<IssueRef>,
        /// Make the issue top-level.
        #[arg(long)] no_parent: bool,
        /// Move it to this project, by name (ignoring case) or id. A milestone
        /// it has in another project is cleared, unless --milestone is given.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name, conflicts_with = "no_project")] project: Option<String>,
        /// Take it out of its project, and so off its milestone.
        #[arg(long, conflicts_with = "milestone")] no_project: bool,
        /// Its milestone, by name (ignoring case) or id: one in the issue's
        /// project (the --project one, or the one it is in).
        #[arg(long, value_name = "MILESTONE", value_parser = parse_milestone_name, conflicts_with = "no_milestone")] milestone: Option<String>,
        /// Take it off its milestone, leaving its project.
        #[arg(long)] no_milestone: bool,
        /// Set the id it has elsewhere, as BZR-1532 (see issue create --ref).
        #[arg(long = "ref", value_name = "REF", value_parser = parse_ref, conflicts_with = "no_ref")] issue_ref: Option<String>,
        /// Remove its ref.
        #[arg(long)] no_ref: bool,
    },
    /// Delete an issue, with its comments and relations. Refused while it has sub-issues.
    Delete {
        /// The issue: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)]
        id: IssueRef,
    },
    /// Relate two issues: A blocks B, A blocked-by B, A related B, or A duplicate-of B.
    ///
    /// A blocked-by B is the same relation as B blocks A, and related goes
    /// both ways; making a relation that exists, in either form, does nothing.
    /// An issue is a duplicate of at most one issue, and duplicate-of sets its
    /// status to Duplicate. A relation that would make a cycle of blocks, or
    /// of duplicates, is refused.
    Relate {
        /// The first issue, A: KEY-N, #N, N or a UUID.
        #[arg(allow_negative_numbers = true)] a: IssueRef,
        /// blocks, blocked-by, related or duplicate-of.
        kind: RelationKind,
        /// The second issue, B: KEY-N, #N, N or a UUID.
        #[arg(allow_negative_numbers = true)] b: IssueRef,
    },
    /// Remove a relation, given in either form (B blocks A removes A blocked-by B).
    ///
    /// Statuses stay as they are: unrelating duplicate-of leaves A Duplicate.
    Unrelate {
        /// The first issue, A: KEY-N, #N, N or a UUID.
        #[arg(allow_negative_numbers = true)] a: IssueRef,
        /// blocks, blocked-by, related or duplicate-of.
        kind: RelationKind,
        /// The second issue, B: KEY-N, #N, N or a UUID.
        #[arg(allow_negative_numbers = true)] b: IssueRef,
    },
    /// Add a comment: BODY inline, or --body-file PATH. Exactly one.
    #[command(group(ArgGroup::new("body_source").required(true).args(["body", "body_file"])))]
    Comment {
        /// The issue: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)] id: IssueRef,
        /// The comment inline. For long text, use --body-file.
        #[arg(value_parser = inline_body)] body: Option<String>,
        /// Read the comment from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_comment)] body_file: Option<String>,
        /// Who is writing it. Without it: LCL_AUTHOR, else USER.
        #[arg(long, value_name = "NAME", value_parser = parse_author)] author: Option<String>,
    },
    /// List an issue's comments, oldest first.
    Comments {
        /// The issue: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)]
        id: IssueRef,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Create a project in this workspace.
    ///
    /// (lcl 0.1.0's project create --global made a workspace; that is now
    /// lcl workspace create.)
    Create {
        /// The project's name: unique in this workspace, ignoring case.
        #[arg(value_name = "NAME", value_parser = parse_project_name)] name: String,
        /// The description inline. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        /// Backlog, Planned, Started (or In Progress), Paused, Completed or Canceled; case and spaces are ignored.
        #[arg(long, default_value = "Backlog")] status: ProjectStatus,
        /// When it should be done, as YYYY-MM-DD.
        #[arg(long, value_name = "DATE", value_parser = parse_date)] target_date: Option<NaiveDate>,
        /// lcl 0.1.0's flag for a global workspace: refused with a hint.
        #[arg(long, hide = true)] global: bool,
        /// lcl 0.1.0's workspace key: refused with a hint.
        #[arg(long, hide = true, num_args = 0..=1, default_missing_value = "", allow_hyphen_values = true)] key: Option<String>,
    },
    /// List projects, by name.
    List {
        /// Only projects with this status; repeat for any of several.
        #[arg(long, value_name = "STATUS")] status: Vec<ProjectStatus>,
    },
    /// Show a project: its fields, its issues counted by status, and its
    /// milestones by sort order, then name.
    Show {
        /// The project's name (ignoring case) or id.
        #[arg(value_name = "PROJECT", value_parser = parse_project_name)] project: String,
    },
    /// Change a project's name, description, status or target date.
    Update {
        /// The project's name (ignoring case) or id.
        #[arg(value_name = "PROJECT", value_parser = parse_project_name)] project: String,
        /// Its new name: unique in this workspace, ignoring case (its own name in another case is fine).
        #[arg(long, value_name = "NAME", value_parser = parse_project_name)] name: Option<String>,
        /// The description inline; "" clears it. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        /// Backlog, Planned, Started (or In Progress), Paused, Completed or Canceled; case and spaces are ignored.
        #[arg(long)] status: Option<ProjectStatus>,
        /// When it should be done, as YYYY-MM-DD.
        #[arg(long, value_name = "DATE", value_parser = parse_date, conflicts_with = "no_target_date")] target_date: Option<NaiveDate>,
        /// Clear the target date.
        #[arg(long)] no_target_date: bool,
    },
    /// Delete a project. Refused while it has issues or milestones.
    Delete {
        /// The project's name (ignoring case) or id.
        #[arg(value_name = "PROJECT", value_parser = parse_project_name)] project: String,
    },
}

#[derive(Subcommand)]
enum MilestoneCmd {
    /// Create a milestone in a project. It stays in that project.
    Create {
        /// The milestone's name: unique in its project, ignoring case.
        #[arg(value_name = "NAME", value_parser = parse_milestone_name)] name: String,
        /// Its project, by name (ignoring case) or id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)] project: String,
        /// The description inline. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        /// When it should be reached, as YYYY-MM-DD.
        #[arg(long, value_name = "DATE", value_parser = parse_date)] target_date: Option<NaiveDate>,
        /// Its place among its project's milestones: lower first, ties by name. May be negative.
        #[arg(long, value_name = "N", default_value_t = 0, allow_negative_numbers = true)] sort_order: i64,
    },
    /// List milestones, by project name, then sort order, then name.
    List {
        /// Only this project's milestones, by name (ignoring case) or id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)] project: Option<String>,
    },
    /// Show a milestone and its issues counted by status.
    Show {
        /// The milestone's name (ignoring case) or id. A name several projects have needs --project.
        #[arg(value_name = "MILESTONE", value_parser = parse_milestone_name)] milestone: String,
        /// The project to look in, by name (ignoring case) or id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)] project: Option<String>,
    },
    /// Change a milestone's name, description, target date or sort order. It can't move to another project.
    Update {
        /// The milestone's name (ignoring case) or id. A name several projects have needs --project.
        #[arg(value_name = "MILESTONE", value_parser = parse_milestone_name)] milestone: String,
        /// The project to look in, by name (ignoring case) or id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)] project: Option<String>,
        /// Its new name: unique in its project, ignoring case (its own name in another case is fine).
        #[arg(long, value_name = "NAME", value_parser = parse_milestone_name)] name: Option<String>,
        /// The description inline; "" clears it. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        /// When it should be reached, as YYYY-MM-DD.
        #[arg(long, value_name = "DATE", value_parser = parse_date, conflicts_with = "no_target_date")] target_date: Option<NaiveDate>,
        /// Clear the target date.
        #[arg(long)] no_target_date: bool,
        /// Its place among its project's milestones: lower first, ties by name. May be negative.
        #[arg(long, value_name = "N", allow_negative_numbers = true)] sort_order: Option<i64>,
    },
    /// Delete a milestone. Refused while issues have it.
    Delete {
        /// The milestone's name (ignoring case) or id. A name several projects have needs --project.
        #[arg(value_name = "MILESTONE", value_parser = parse_milestone_name)] milestone: String,
        /// The project to look in, by name (ignoring case) or id.
        #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)] project: Option<String>,
    },
}

#[derive(Subcommand)]
enum DocCmd {
    /// Create a doc, at the top level or under another doc.
    Create {
        /// The doc's title.
        #[arg(long)] title: String,
        /// The content inline. For long text, use --content-file.
        #[arg(long, value_parser = inline_content, conflicts_with = "content_file")] content: Option<String>,
        /// Read the content from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_content)] content_file: Option<String>,
        /// The parent doc's id: it must exist.
        #[arg(long, value_name = "ID")] parent: Option<String>,
    },
    /// List docs: id and title, most recently updated first.
    List,
    /// Show a doc: its title and content.
    Show {
        /// The doc's id, as doc list shows it.
        id: String,
    },
    /// Change a doc's title, content or parent: only what the flags name changes.
    Update {
        /// The doc's id, as doc list shows it.
        id: String,
        /// Its new title.
        #[arg(long)] title: Option<String>,
        /// The content inline; "" clears it. For long text, use --content-file.
        #[arg(long, value_parser = inline_content, conflicts_with = "content_file")] content: Option<String>,
        /// Read the content from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_content)] content_file: Option<String>,
        /// Move it under the doc with this id, which must exist and must not
        /// be this doc or one below it.
        #[arg(long, value_name = "ID", conflicts_with = "no_parent")] parent: Option<String>,
        /// Make it a top-level doc.
        #[arg(long)] no_parent: bool,
    },
    /// Delete a doc.
    Delete {
        /// The doc's id, as doc list shows it.
        id: String,
    },
}

#[derive(Subcommand)]
enum LabelCmd {
    /// Create a label.
    Create {
        /// The label's name, trimmed: not blank, and unique ignoring case.
        #[arg(value_parser = parse_label_name)]
        name: String,
        /// Its color, as #rgb or #rrggbb.
        #[arg(long, default_value = "#888888", value_parser = parse_color)]
        color: String,
    },
    /// List labels: id, name and color, by name.
    List,
    /// Delete a label by name (ignoring case) or id, removing it from every issue.
    Delete {
        /// The label's name (ignoring case) or id.
        #[arg(value_parser = parse_label_name)]
        name_or_id: String,
    },
}

#[derive(Subcommand)]
enum CycleCmd {
    /// Create a cycle, with or without its dates.
    Create {
        /// The cycle's name.
        name: String,
        /// Its first day, as YYYY-MM-DD.
        #[arg(long, value_parser = parse_date)]
        start: Option<NaiveDate>,
        /// Its last day, as YYYY-MM-DD: not before --start.
        #[arg(long, value_parser = parse_date)]
        end: Option<NaiveDate>,
    },
    /// List cycles: id, name and dates, newest first.
    List,
}

// The value rules live in lcl_core::parse, shared with `import`; these
// adapt them to clap.

/// A real calendar date written exactly as YYYY-MM-DD.
fn parse_date(s: &str) -> std::result::Result<NaiveDate, String> { parse::date(s).map_err(|e| e.0) }

/// An estimate: a whole number, 0 or more.
fn parse_estimate(s: &str) -> std::result::Result<i64, String> {
    let n: i64 = s.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    if n < 0 {
        return Err(format!("estimate must be 0 or more; got {n}"));
    }
    Ok(n)
}

/// `#rgb` or `#rrggbb`.
fn parse_color(s: &str) -> std::result::Result<String, String> { parse::color(s).map_err(|e| e.0) }

// Long text. Inline text passes through the shell, which splits, expands and
// substitutes it, and through argv, which has a size limit. So text can also
// come from a file or stdin, read here as clap parses: every problem exits 2
// before any database is opened.

/// Inline text, unless it is a bare `-`: whoever writes that means stdin,
/// and storing "-" is never what they want.
fn not_dash(s: &str, file_flag: &str) -> std::result::Result<String, String> {
    if s == "-" { Err(format!("to read stdin, use {file_flag} -")) } else { Ok(s.to_string()) }
}
fn inline_description(s: &str) -> std::result::Result<String, String> { not_dash(s, "--description-file") }
fn inline_content(s: &str) -> std::result::Result<String, String> { not_dash(s, "--content-file") }
/// A comment inline: not `-`, and not blank (empty or only whitespace).
fn inline_body(s: &str) -> std::result::Result<String, String> {
    if s.trim().is_empty() { Err("a comment can't be blank".into()) } else { not_dash(s, "--body-file") }
}

fn read_description(path: &str) -> std::result::Result<String, String> { read_text(path, "--description-file") }
fn read_content(path: &str) -> std::result::Result<String, String> { read_text(path, "--content-file") }
/// A comment from a file or stdin: as `read_text`, and not blank. A comment
/// that isn't blank is still stored byte for byte.
fn read_comment(path: &str) -> std::result::Result<String, String> {
    let body = read_text(path, "--body-file")?;
    if body.trim().is_empty() {
        return Err(format!("{} is blank; a comment can't be blank", if path == "-" { "stdin" } else { path }));
    }
    Ok(body)
}

/// The text in file `path`, or on stdin if `path` is `-`, byte for byte: no
/// trimming, no size cap. Refuses an unreadable file and bytes that aren't
/// UTF-8. `flag` names the option, for the stdin hint.
fn read_text(path: &str, flag: &str) -> std::result::Result<String, String> {
    let (name, bytes) = if path == "-" {
        ("stdin", read_stdin(flag)?)
    } else {
        (path, std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?)
    };
    String::from_utf8(bytes).map_err(|e| format!("{name} is not valid UTF-8: invalid byte at offset {}", e.utf8_error().valid_up_to()))
}

/// How long stdin may stay silent before its first byte or EOF:
/// LCL_STDIN_WAIT seconds, default 10. `None` (from 0) waits forever.
fn stdin_wait() -> std::result::Result<Option<Duration>, String> {
    let Ok(v) = std::env::var("LCL_STDIN_WAIT") else { return Ok(Some(Duration::from_secs(10))) };
    match v.trim().parse::<u64>() {
        Ok(0) => Ok(None),
        Ok(secs) => Ok(Some(Duration::from_secs(secs))),
        Err(_) => Err(format!("LCL_STDIN_WAIT must be a whole number of seconds (0 waits forever), not {v:?}")),
    }
}

/// All of stdin. A terminal is refused at once. A pipe or socket that stays
/// silent is refused after `stdin_wait()`: an agent's shell can hand lcl a
/// stdin that never sends EOF, and waiting on it would hang the agent. Once
/// the first byte or EOF arrives, the rest is read with no limit, so a
/// producer that pauses mid-stream still works.
///
/// The read blocks in a thread of its own; on a timeout, the process exits
/// (clap's error path) and takes the blocked thread with it.
fn read_stdin(flag: &str) -> std::result::Result<Vec<u8>, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("nothing is piped to stdin".into());
    }
    let wait = stdin_wait()?;
    enum Msg {
        Started,
        Done(std::io::Result<Vec<u8>>),
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut chunk = vec![0; 64 * 1024];
        let first = loop {
            match stdin.read(&mut chunk) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                r => break r,
            }
        };
        let _ = tx.send(Msg::Started);
        let all = first.and_then(|n| {
            chunk.truncate(n);
            if n > 0 {
                stdin.read_to_end(&mut chunk)?;
            }
            Ok(chunk)
        });
        let _ = tx.send(Msg::Done(all));
    });
    let started = match wait {
        Some(w) => rx.recv_timeout(w).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => format!(
                "nothing arrived on stdin within {} s; pipe the text in or use a heredoc: … {flag} - <<'EOF'",
                w.as_secs()
            ),
            std::sync::mpsc::RecvTimeoutError::Disconnected => "can't read stdin".to_string(),
        })?,
        None => rx.recv().map_err(|_| "can't read stdin".to_string())?,
    };
    debug_assert!(matches!(started, Msg::Started));
    match rx.recv() {
        Ok(Msg::Done(all)) => all.map_err(|e| format!("can't read stdin: {e}")),
        _ => Err("can't read stdin".into()),
    }
}

/// The search words joined with one space, made safe for FTS5 (see
/// `lcl_core::search`). A query with nothing to search for exits 2 as a clap
/// error does, with search's usage line, before a database is opened.
fn search_query(words: &[String]) -> SearchQuery {
    let joined = words.join(" ");
    SearchQuery::parse(&joined).unwrap_or_else(|e| {
        let mut cli = Cli::command();
        cli.build();
        let search = cli.find_subcommand_mut("search").expect("the search command");
        search.error(clap::error::ErrorKind::ValueValidation, format!("invalid value '{joined}' for '<WORD>...': {e}")).exit()
    })
}

/// A label name, trimmed; blank is refused.
fn parse_label_name(s: &str) -> std::result::Result<String, String> { parse::label_name(s).map_err(|e| e.0) }

/// An assignee, trimmed; blank is refused. Unassigning is its own flag.
fn parse_assignee(s: &str) -> std::result::Result<String, String> { parse::assignee(s).map_err(|e| e.0) }

/// `issue list --assignee`: a name, trimmed, or `none` (any case) for
/// unassigned issues. Blank is refused.
fn parse_assignee_filter(s: &str) -> std::result::Result<AssigneeFilter, String> {
    let s = s.trim();
    if s.is_empty() { Err("an assignee can't be empty; for unassigned issues, use --assignee none".into()) }
    else if s.eq_ignore_ascii_case("none") { Ok(AssigneeFilter::Unassigned) }
    else { Ok(AssigneeFilter::Named(s.to_string())) }
}

/// `--ref`: trimmed; at most 100 characters, none of them whitespace or a
/// control character. Blank is refused.
fn parse_ref(s: &str) -> std::result::Result<String, String> { parse::issue_ref(s).map_err(|e| e.0) }

/// `--author`: trimmed; blank is refused (leaving it out means LCL_AUTHOR,
/// else USER).
fn parse_author(s: &str) -> std::result::Result<String, String> { parse::author(s).map_err(|e| e.0) }

/// Who is writing, when --author isn't given: LCL_AUTHOR, else USER,
/// trimmed. Empty (or only whitespace, or not UTF-8) counts as unset; with
/// neither, nobody (null).
fn env_author() -> Option<String> {
    ["LCL_AUTHOR", "USER"].iter().find_map(|v| std::env::var(v).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// A project name or reference, trimmed; blank is refused.
fn parse_project_name(s: &str) -> std::result::Result<String, String> { parse::project_name(s).map_err(|e| e.0) }

/// A milestone name or reference, trimmed; blank is refused.
fn parse_milestone_name(s: &str) -> std::result::Result<String, String> { parse::milestone_name(s).map_err(|e| e.0) }

/// `issue list --project`: a project, or `none` (any case) for issues in no
/// project.
fn parse_project_filter(s: &str) -> std::result::Result<ProjectFilter, String> {
    let s = parse_project_name(s)?;
    if s.eq_ignore_ascii_case("none") { Ok(ProjectFilter::NoProject) } else { Ok(ProjectFilter::Is(s)) }
}

/// `workspace create NAME`: trimmed, and with a letter or digit, so that it
/// has a slug to name its file.
fn parse_workspace_name(s: &str) -> std::result::Result<String, String> {
    let s = s.trim();
    match registry::slug(s) {
        Some(_) => Ok(s.to_string()),
        None => Err(format!("workspace name {s:?} has no letters or digits")),
    }
}

/// `args` as a shell would need them typed.
fn shell_line(args: &[String]) -> String {
    let word = |s: &String| {
        let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._/:@%+=,-".contains(c));
        if plain { s.clone() } else { format!("'{}'", s.replace('\'', r"'\''")) }
    };
    args.iter().map(word).collect::<Vec<_>>().join(" ")
}

/// Refuses the commands that were renamed (exit 2, before anything is
/// opened), with the new form.
fn refuse_old_forms(cli: &Cli) {
    let fail = |msg: String| -> ! { clap_exit(Cli::command().error(clap::error::ErrorKind::InvalidSubcommand, msg)) };
    if cli.project.is_some() {
        // The same command line with the first --project (the one before
        // the subcommand; clap took no other) renamed.
        let mut renamed = false;
        let args: Vec<String> = std::env::args().skip(1).map(|a| {
            if renamed { return a; }
            if a == "--project" { renamed = true; "--workspace".to_string() }
            else if let Some(v) = a.strip_prefix("--project=") { renamed = true; format!("--workspace={v}") }
            else { a }
        }).collect();
        fail(format!("the workspace flag is now --workspace: lcl {}", shell_line(&args)));
    }
    // lcl 0.1.0's project create made a workspace when given either flag.
    if let Commands::Project { cmd: ProjectCmd::Create { global, key, .. } } = &cli.cmd
        && (*global || key.is_some())
    {
        fail("workspaces are now: lcl workspace create NAME --key KEY / list / show".into());
    }
}

/// An environment variable with a non-empty value. Empty counts as unset.
fn env_set(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// Parses the command line. An empty LCL_WORKSPACE counts as unset: clap
/// would pass it through as the name "". An empty `-w ""` typed on the
/// command line stays, and is refused later (exit 2).
fn parse() -> Cli {
    let matches = Cli::command().try_get_matches().unwrap_or_else(|e| clap_exit(e));
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| clap_exit(e));
    if matches.value_source("workspace") == Some(ValueSource::EnvVariable) && cli.workspace.as_deref() == Some("") {
        cli.workspace = None;
    }
    cli
}

/// LCL_PROJECT, the old name of LCL_WORKSPACE, still selects a workspace
/// when LCL_WORKSPACE is unset (and --workspace isn't given), with a notice
/// on every run while it is set. Empty counts as unset for both.
fn honour_lcl_project(cli: &mut Cli) {
    if env_set("LCL_WORKSPACE").is_some() {
        return;
    }
    if let Some(old) = env_set("LCL_PROJECT") {
        eprintln!("lcl: LCL_PROJECT is deprecated; use LCL_WORKSPACE");
        cli.workspace.get_or_insert_with(|| old.to_string_lossy().into_owned());
    }
}

/// Checks that span more than one argument. clap's value parsers have
/// already checked each value on its own; this exits 2 the same way, with
/// the usage of the subcommand at `path` (as clap's own errors have).
fn validate(cli: &Cli) {
    let fail = |path: &[&str], msg: String| -> ! { clap_exit(subcommand_error(path, msg)) };
    if let Commands::Cycle { cmd: CycleCmd::Create { start: Some(start), end: Some(end), .. } } = &cli.cmd
        && end < start
    {
        fail(&["cycle", "create"], format!("cycle end {end} is before its start {start}"));
    }
    // Names match ignoring ASCII case, as label lookups do.
    if let Commands::Issue { cmd: IssueCmd::Update { add_label, remove_label, .. } } = &cli.cmd
        && let Some(both) = add_label.iter().find(|a| remove_label.iter().any(|r| r.eq_ignore_ascii_case(a)))
    {
        fail(&["issue", "update"], format!("label {both:?} is in both --add-label and --remove-label"));
    }
    if let Commands::Issue { cmd: IssueCmd::List { project: Some(ProjectFilter::NoProject), milestone: Some(m), .. } } = &cli.cmd {
        fail(&["issue", "list"], LclError::MilestoneInNoProject(m.clone()).to_string());
    }
}

/// `r`, except that a --project value (`flag`) naming no project gets a
/// hint when it is a registered workspace's name: lcl 0.1.0's --project
/// selected a workspace. The registry is read here without being created
/// or upgraded; with HOME unset there is no hint.
fn project_hint<T>(r: lcl_core::error::Result<T>, flag: Option<&str>) -> Result<T> {
    match r {
        Err(LclError::ProjectNotFound { name, .. }) if flag == Some(name.as_str()) => {
            let workspace = match (home(), registry::slug(&name)) {
                (Ok(home), Some(slug)) => registry::is_registered(&home, &slug),
                _ => false,
            };
            Err(LclError::ProjectNotFound { name, workspace }.into())
        }
        r => Ok(r?),
    }
}

/// Placement from an issue command's four flags (clap keeps each pair apart).
fn placement(project: Option<String>, no_project: bool, milestone: Option<String>, no_milestone: bool) -> Placement {
    let change = |set: Option<String>, clear: bool| match (set, clear) {
        (Some(v), _) => Change::Set(v),
        (None, true) => Change::Clear,
        (None, false) => Change::Keep,
    };
    Placement { project: change(project, no_project), milestone: change(milestone, no_milestone) }
}

/// "12 (Backlog 3, Done 9)": the total, then each non-zero status, in
/// Status order.
fn counts_line(c: &IssueCounts) -> String {
    let parts: Vec<String> = c.by_status.entries().iter().filter(|(_, n)| *n != 0).map(|(k, n)| format!("{k} {n}")).collect();
    if parts.is_empty() { c.total.to_string() } else { format!("{} ({})", c.total, parts.join(", ")) }
}

/// $HOME, where lcl keeps ~/.lcl. Unset or empty is an error, never a
/// fallback directory; only what needs ~/.lcl asks for it, so a workspace
/// in the current directory works without HOME.
fn home() -> Result<PathBuf, LclError> {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => Ok(PathBuf::from(h)),
        _ => Err(LclError::NoHome),
    }
}

fn find_local_db() -> Option<PathBuf> {
    let mut cur = std::env::current_dir().ok()?;
    loop {
        let cand = cur.join(".lcl").join("data.sqlite");
        if cand.exists() { return Some(cand); }
        if !cur.pop() { break; }
    }
    None
}

/// The workspace a command works in: the registered one --workspace names
/// (see `registry::resolve`), or else the nearest .lcl/data.sqlite at or
/// above the current directory. Never creates one; the registry is read
/// only for --workspace, and only if it exists.
fn resolve_workspace(workspace: Option<&str>) -> Result<(PathBuf, Store)> {
    let path = match workspace {
        Some(name) => {
            let slug = registry::slug(name).ok_or_else(|| LclError::NoSlug(name.to_string()))?;
            let path = registry::resolve(&home()?, &slug)?;
            if !path.exists() {
                return Err(LclError::WorkspaceNotFound { name: name.to_string(), path: path.display().to_string() }.into());
            }
            path
        }
        None => find_local_db().ok_or(LclError::NoWorkspace)?,
    };
    let store = open_store(&path).with_context(|| format!("open workspace {}", path.display()))?;
    Ok((path, store))
}

/// Opens a workspace file, and when that upgraded it and the upgrade changed
/// values, says so on stderr. The upgrade runs once, so the line shows once.
fn open_store(path: &std::path::Path) -> lcl_core::error::Result<Store> {
    let store = Store::open(path)?;
    if let Some(note) = store.upgrade_note() {
        eprintln!("lcl: {note}");
    }
    Ok(store)
}

fn resolve_store(workspace: &Option<String>) -> Result<Store> {
    resolve_workspace(workspace.as_deref()).map(|(_, store)| store)
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(v)?;
    writeln!(std::io::stdout(), "{text}").map_err(|e| stdout_failed(e, "can't write to stdout"))
}

/// A write to stdout failed because its reader went away (`lcl ... | head`).
/// It ends lcl quietly with exit 0: the reader has what it wanted.
#[derive(Debug)]
struct StdoutClosed;

impl std::fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("stdout was closed")
    }
}

impl std::error::Error for StdoutClosed {}

/// The error for a failed write to stdout: StdoutClosed for a broken pipe,
/// else the error with `what` as its context.
fn stdout_failed(e: std::io::Error, what: &'static str) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::BrokenPipe { anyhow::Error::new(StdoutClosed) } else { anyhow::Error::new(e).context(what) }
}

/// Whether errors print as JSON: `--json` is one of the arguments before
/// any `--`. Read from the arguments themselves, so it holds for the errors
/// clap finds before it has parsed anything.
fn json_errors() -> bool {
    std::env::args_os().skip(1).take_while(|a| a != "--").any(|a| a == "--json")
}

/// An error under --json: `{"error":{"kind":K,"message":M}}`, one line on
/// stderr.
fn print_json_error(kind: ErrorKind, message: &str) {
    let line = serde_json::json!({"error": {"kind": kind.as_str(), "message": message}});
    eprintln!("{line}");
}

/// Ends lcl with a clap error. --help and --version print as clap prints
/// them (exit 0), and so does every error without --json (exit 2). Under
/// --json an error is one JSON line, of kind usage, exit 2.
fn clap_exit(e: clap::Error) -> ! {
    if e.exit_code() == 0 || !json_errors() {
        e.exit()
    }
    // A group given no subcommand (`lcl issue`) gets its help printed, which
    // is no message for a program: it gets clap's MissingSubcommand error.
    let e = if e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand { missing_subcommand().unwrap_or(e) } else { e };
    print_json_error(ErrorKind::Usage, &clap_message(&e));
    std::process::exit(2)
}

/// The error clap gives a group with no subcommand when it doesn't print
/// the group's help instead ("'lcl issue' requires a subcommand but one was
/// not provided", and the subcommands): from parsing the same arguments
/// again with that help turned off.
fn missing_subcommand() -> Option<clap::Error> {
    fn no_help_instead(cmd: clap::Command) -> clap::Command {
        cmd.arg_required_else_help(false).mut_subcommands(no_help_instead)
    }
    no_help_instead(Cli::command()).try_get_matches().err()
}

/// clap's text for an error, without its "error: " prefix, its usage, and
/// its "For more information" line.
fn clap_message(e: &clap::Error) -> String {
    let text = e.render().to_string();
    let kept: Vec<&str> = text.split("\n\n").filter(|part| !part.starts_with("Usage:") && !part.starts_with("For more information")).collect();
    let message = kept.join("\n\n");
    let message = message.trim_end();
    message.strip_prefix("error: ").unwrap_or(message).to_string()
}

/// A clap error from the subcommand at `path` (`["cycle", "create"]`), so
/// that it shows that subcommand's usage, not lcl's.
fn subcommand_error(path: &[&str], msg: String) -> clap::Error {
    let mut cmd = Cli::command();
    cmd.build();
    let mut sub = &mut cmd;
    for name in path {
        sub = sub.find_subcommand_mut(name).expect("a subcommand of lcl");
    }
    sub.error(clap::error::ErrorKind::ValueValidation, msg)
}

/// The kind of an error from `run`.
fn error_kind(e: &anyhow::Error) -> ErrorKind {
    if let Some(e) = e.downcast_ref::<LclError>() {
        return e.kind();
    }
    if e.chain().any(|c| c.is::<std::io::Error>()) { ErrorKind::Io } else { ErrorKind::Other }
}

/// Exit 2 for a usage error, as clap does for a bad argument; 1 for anything
/// else. Errors print as `fn main() -> Result` would print them, or, under
/// --json, as one JSON line.
fn main() -> ExitCode {
    let mut cli = parse();
    refuse_old_forms(&cli);
    honour_lcl_project(&mut cli);
    validate(&cli);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.downcast_ref::<StdoutClosed>().is_some() => ExitCode::SUCCESS,
        Err(e) => {
            let kind = error_kind(&e);
            if json_errors() { print_json_error(kind, &format!("{e:?}")); } else { eprintln!("Error: {e:?}"); }
            ExitCode::from(if kind == ErrorKind::Usage { 2 } else { 1 })
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Commands::Init { name, key } => {
            // Every check that needs nothing opened comes first, so a
            // refused init creates nothing: not .lcl/, not ~/.lcl.
            let home = home()?;
            let cwd = std::env::current_dir()?;
            let name = match name {
                Some(name) => name,
                None => cwd.file_name().map(|s| s.to_string_lossy().trim().to_string()).ok_or(LclError::InitNeedsName)?,
            };
            if registry::slug(&name).is_none() {
                return Err(LclError::NoSlug(name).into());
            }
            let dbpath = cwd.join(".lcl").join("data.sqlite");
            let made = Registry::open(&home)?.init_workspace(&name, &key, &dbpath, open_store)?;
            if cli.json { print_json(&made)?; }
            else if made.created { outln!("Initialized workspace '{}' at {}", made.name, made.path); }
            else { outln!("Workspace {:?} (key {}) already exists at {}", made.name, made.key, made.path); }
        }
        Commands::Project { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                ProjectCmd::Create { name, description, description_file, status, target_date, global: _, key: _ } => {
                    let description = description.or(description_file).unwrap_or_default();
                    let p = store.create_project(&name, &description, status, target_date.map(|d| d.to_string()).as_deref())?;
                    if cli.json { print_json(&p)?; } else { outln!("Created project {:?}", p.name); }
                }
                ProjectCmd::List { status } => {
                    let all = store.list_projects(&status)?;
                    if cli.json { print_json(&all)?; }
                    else { for p in &all { outln!("{}\t[{}]\t{}", p.name, p.status, p.target_date.as_deref().unwrap_or("")); } }
                }
                ProjectCmd::Show { project } => {
                    let d = store.project_detail(&project)?;
                    if cli.json { print_json(&d)?; }
                    else {
                        let p = &d.project;
                        outln!("{} [{}]{}", p.name, p.status, p.target_date.as_ref().map(|t| format!("  target {t}")).unwrap_or_default());
                        if !p.description.is_empty() { outln!("\n{}\n", p.description); }
                        outln!("Issues: {}", counts_line(&d.issues));
                        if !d.milestones.is_empty() {
                            outln!("Milestones:");
                            for m in &d.milestones {
                                outln!("  {}\t{}\t{} issue{}", m.name, m.target_date.as_deref().unwrap_or(""), m.issues, if m.issues == 1 { "" } else { "s" });
                            }
                        }
                    }
                }
                ProjectCmd::Update { project, name, description, description_file, status, target_date, no_target_date } => {
                    let u = ProjectUpdate {
                        name,
                        description: description.or(description_file),
                        status,
                        target_date: match (target_date, no_target_date) {
                            (Some(d), _) => Change::Set(d.to_string()),
                            (None, true) => Change::Clear,
                            (None, false) => Change::Keep,
                        },
                    };
                    let p = store.update_project(&project, &u)?;
                    if cli.json { print_json(&p)?; } else { outln!("Updated project {:?}", p.name); }
                }
                ProjectCmd::Delete { project } => {
                    let p = store.delete_project(&project)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": p.name, "id": p.id}))?; } else { outln!("Deleted project {:?}", p.name); }
                }
            }
        }
        Commands::Milestone { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                MilestoneCmd::Create { name, project, description, description_file, target_date, sort_order } => {
                    let description = description.or(description_file).unwrap_or_default();
                    let target = target_date.map(|d| d.to_string());
                    let m = project_hint(store.create_milestone(&name, &project, &description, target.as_deref(), sort_order), Some(&project))?;
                    if cli.json { print_json(&m)?; } else { outln!("Created milestone {:?} in project {:?}", m.name, m.project); }
                }
                MilestoneCmd::List { project } => {
                    let all = project_hint(store.list_milestones(project.as_deref()), project.as_deref())?;
                    if cli.json { print_json(&all)?; }
                    else { for m in &all { outln!("{}\t{}\t{}\t{}", m.project, m.sort_order, m.name, m.target_date.as_deref().unwrap_or("")); } }
                }
                MilestoneCmd::Show { milestone, project } => {
                    let d = project_hint(store.milestone_detail(&milestone, project.as_deref()), project.as_deref())?;
                    if cli.json { print_json(&d)?; }
                    else {
                        let m = &d.milestone;
                        outln!("{} (project {:?}){}", m.name, m.project, m.target_date.as_ref().map(|t| format!("  target {t}")).unwrap_or_default());
                        if !m.description.is_empty() { outln!("\n{}\n", m.description); }
                        outln!("Issues: {}", counts_line(&d.issues));
                    }
                }
                MilestoneCmd::Update { milestone, project, name, description, description_file, target_date, no_target_date, sort_order } => {
                    let u = MilestoneUpdate {
                        name,
                        description: description.or(description_file),
                        target_date: match (target_date, no_target_date) {
                            (Some(d), _) => Change::Set(d.to_string()),
                            (None, true) => Change::Clear,
                            (None, false) => Change::Keep,
                        },
                        sort_order,
                    };
                    let m = project_hint(store.update_milestone(&milestone, project.as_deref(), &u), project.as_deref())?;
                    if cli.json { print_json(&m)?; } else { outln!("Updated milestone {:?} in project {:?}", m.name, m.project); }
                }
                MilestoneCmd::Delete { milestone, project } => {
                    let m = project_hint(store.delete_milestone(&milestone, project.as_deref()), project.as_deref())?;
                    if cli.json { print_json(&serde_json::json!({"deleted": m.name, "id": m.id, "project": m.project}))?; }
                    else { outln!("Deleted milestone {:?} from project {:?}", m.name, m.project); }
                }
            }
        }
        Commands::Workspace { cmd } => match cmd {
            WorkspaceCmd::Create { name, key } => {
                let made = Registry::open(&home()?)?.create_workspace(&name, &key)?;
                if cli.json { print_json(&made)?; }
                else { outln!("Created workspace {:?} (key {}) at {}", made.name, made.key, made.path); }
            }
            WorkspaceCmd::List => {
                let all = Registry::open(&home()?)?.list()?;
                if cli.json { print_json(&all)?; }
                else {
                    for w in &all { outln!("{}\t{}\t{}{}", w.name, w.slug, w.path, if w.missing { "\t(missing)" } else { "" }); }
                }
            }
            WorkspaceCmd::Show => {
                let (path, store) = resolve_workspace(cli.workspace.as_deref())?;
                let name = store.get_meta("name")?.unwrap_or_default();
                let key = store.get_meta("key")?.unwrap_or_default();
                // Stored as text; null if absent or not a number.
                let created_at = store.get_meta("created_at")?.and_then(|t| t.parse::<i64>().ok());
                let description = store.get_meta("description")?.unwrap_or_default();
                let schema_version = store.schema_version()?;
                let v = serde_json::json!({"name": name, "key": key, "description": description, "path": path, "schema_version": schema_version, "created_at": created_at});
                if cli.json { print_json(&v)?; } else { outln!("{} [{}] {}", name, key, path.display()); }
            }
        },
        Commands::Issue { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                IssueCmd::Create {
                    title, description, description_file, status, priority, assignee, parent, label, estimate, due,
                    project, no_project, milestone, no_milestone, issue_ref, author,
                } => {
                    let description = description.or(description_file).unwrap_or_default();
                    let parent_id = parent.map(|p| store.resolve(&p)).transpose()?.map(|p| p.id);
                    let flag = project.clone();
                    let place = placement(project, no_project, milestone, no_milestone);
                    let issue = project_hint(store.create_issue(Issue {
                        id: String::new(), number: 0, identifier: String::new(), title, description, status, priority,
                        assignee, parent_id, parent_identifier: None, project_id: None, project: None, milestone_id: None, milestone: None,
                        r#ref: issue_ref, creator: author.or_else(env_author), estimate, due_date: due.map(|d| d.to_string()), labels: label,
                        created_at: 0, updated_at: 0, completed_at: None, started_at: None, canceled_at: None,
                    }, &place), flag.as_deref())?;
                    if cli.json { print_json(&issue)?; } else { outln!("Created {}", issue.identifier); }
                }
                IssueCmd::List { status, assignee, label, parent, blocked, unblocked, project, milestone, sort, limit, offset, all } => {
                    let page = if all { Page::ALL } else { Page { limit: Some(limit), offset } };
                    let blocked = if blocked { Some(true) } else if unblocked { Some(false) } else { None };
                    let flag = match &project { Some(ProjectFilter::Is(p)) => Some(p.clone()), _ => None };
                    let filter = IssueFilter { statuses: status, assignee, labels: label, parent, blocked, project, milestone };
                    let list = project_hint(store.list_issues(&filter, sort, page), flag.as_deref())?;
                    if cli.json { print_json(&list)?; }
                    else {
                        for i in &list.issues { outln!("{}\t[{}]\t{}\t{}", i.identifier, i.status, i.priority.label(), i.title); }
                        if list.truncated {
                            let first = list.offset as usize + 1;
                            outln!("showing {first}–{} of {}; use --all, or --limit/--offset", first + list.issues.len() - 1, list.total);
                        }
                    }
                }
                IssueCmd::Show { id } => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(&issue.id)?;
                    let children = store.children(&issue.id)?;
                    let relations = store.relations(&issue.id)?;
                    if cli.json {
                        let children: Vec<&str> = children.iter().map(|c| c.identifier.as_str()).collect();
                        print_json(&serde_json::json!({"issue": issue, "comments": comments, "children": children, "relations": relations}))?;
                    }
                    else {
                        outln!("{} {}", issue.identifier, issue.title);
                        outln!("Status: {}  Priority: {}  Assignee: {}", issue.status, issue.priority.label(), issue.assignee.as_deref().unwrap_or("Unassigned"));
                        if let Some(r) = &issue.r#ref { outln!("Ref: {r}"); }
                        if let Some(c) = &issue.creator { outln!("Creator: {c}"); }
                        if !issue.labels.is_empty() { outln!("Labels: {}", issue.labels.join(", ")); }
                        if let Some(p) = &issue.parent_identifier { outln!("Parent: {p}"); }
                        if let Some(p) = &issue.project { outln!("Project: {p}"); }
                        if let Some(m) = &issue.milestone { outln!("Milestone: {m}"); }
                        let line = |name: &str, others: &[RelatedIssue]| -> Result<()> {
                            if !others.is_empty() {
                                let others: Vec<String> = others.iter().map(|o| format!("{} [{}]", o.identifier, o.status)).collect();
                                outln!("{name}: {}", others.join(", "));
                            }
                            Ok(())
                        };
                        line("Blocks", &relations.blocks)?;
                        line("Blocked by", &relations.blocked_by)?;
                        line("Related", &relations.related)?;
                        line("Duplicate of", relations.duplicate_of.as_slice())?;
                        line("Duplicates", &relations.duplicates)?;
                        if !issue.description.is_empty() { outln!("\n{}", issue.description); }
                        if !children.is_empty() {
                            outln!("\nSub-issues:");
                            for c in &children { outln!("  {} {}", c.identifier, c.title); }
                        }
                        if !comments.is_empty() {
                            outln!("\nComments:");
                            for c in comments { outln!("  - {}: {}", c.author.as_deref().unwrap_or("anon"), c.body); }
                        }
                    }
                }
                IssueCmd::Update {
                    id, title, description, description_file, status, priority, assignee, no_assignee, estimate, no_estimate, due, no_due,
                    label, add_label, remove_label, clear_labels, parent, no_parent, project, no_project, milestone, no_milestone,
                    issue_ref, no_ref,
                } => {
                    // An unknown name is an error even when removing; a known
                    // label the issue doesn't have is a no-op.
                    store.require_labels(&remove_label)?;
                    // Some(new parent UUID or None), or None to leave it alone.
                    let new_parent = match (parent, no_parent) {
                        (_, true) => Some(None),
                        (Some(p), false) => Some(Some(store.resolve(&p)?.id)),
                        (None, false) => None,
                    };
                    let flag = project.clone();
                    let place = placement(project, no_project, milestone, no_milestone);
                    let (issue, cleared) = project_hint(store.update_issue(&id, &place, |i| {
                        if let Some(t) = title { i.title = t; }
                        if let Some(d) = description.or(description_file) { i.description = d; }
                        if let Some(st) = status { i.status = st; }
                        if let Some(p) = priority { i.priority = p; }
                        if assignee.is_some() { i.assignee = assignee; }
                        if no_assignee { i.assignee = None; }
                        if let Some(e) = estimate { i.estimate = Some(e); }
                        if no_estimate { i.estimate = None; }
                        if let Some(d) = due { i.due_date = Some(d.to_string()); }
                        if no_due { i.due_date = None; }
                        // Label edits apply to the set as read inside the write
                        // transaction, so concurrent edits all land.
                        if !label.is_empty() { i.labels = label; }
                        if clear_labels { i.labels.clear(); }
                        i.labels.retain(|n| !remove_label.iter().any(|r| r.eq_ignore_ascii_case(n)));
                        i.labels.extend(add_label);
                        if let Some(p) = new_parent { i.parent_id = p; }
                        if issue_ref.is_some() { i.r#ref = issue_ref; }
                        if no_ref { i.r#ref = None; }
                    }), flag.as_deref())?;
                    if let Some(c) = cleared {
                        eprintln!("lcl: milestone {:?} cleared: it belongs to project {:?}", c.milestone, c.project);
                    }
                    if cli.json { print_json(&issue)?; } else { outln!("Updated {}", issue.identifier); }
                }
                IssueCmd::Delete { id } => {
                    let issue = store.delete_issue(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": issue.identifier, "id": issue.id}))?; } else { outln!("Deleted {}", issue.identifier); }
                }
                IssueCmd::Relate { a, kind, b } => {
                    let (relation, created) = store.relate(&a, kind, &b)?;
                    if cli.json { print_json(&serde_json::json!({"issue": relation.issue, "kind": relation.kind, "related": relation.related, "created": created}))?; }
                    else if !created { outln!("Already related: {relation}"); }
                    else if kind == RelationKind::DuplicateOf { outln!("{relation}; {} is Duplicate", relation.issue); }
                    else { outln!("{relation}"); }
                }
                IssueCmd::Unrelate { a, kind, b } => {
                    let relation = store.unrelate(&a, kind, &b)?;
                    if cli.json { print_json(&serde_json::json!({"issue": relation.issue, "kind": relation.kind, "related": relation.related, "removed": true}))?; }
                    else { outln!("Removed: {relation}"); }
                }
                IssueCmd::Comment { id, body, body_file, author } => {
                    let Some(body) = body.or(body_file) else { unreachable!("clap requires BODY or --body-file") };
                    let c = store.add_comment(&id, body, author.or_else(env_author))?;
                    if cli.json { print_json(&c)?; } else { outln!("Comment added"); }
                }
                IssueCmd::Comments { id } => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(&issue.id)?;
                    if cli.json { print_json(&comments)?; }
                    else { for c in comments { outln!("[{}] {}: {}", c.created_at, c.author.as_deref().unwrap_or("anon"), c.body); } }
                }
            }
        },
        Commands::Doc { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                DocCmd::Create { title, content, content_file, parent } => {
                    let content = content.or(content_file).unwrap_or_default();
                    let d = store.create_doc(Doc { id: String::new(), title, content, parent_id: parent, created_at: 0, updated_at: 0 })?;
                    if cli.json { print_json(&d)?; } else { outln!("Created doc {}", d.id); }
                }
                DocCmd::List => {
                    let docs = store.list_docs()?;
                    if cli.json { print_json(&docs)?; }
                    else { for d in docs { outln!("{}\t{}", d.id, d.title); } }
                }
                DocCmd::Show { id } => {
                    let d = store.get_doc(&id)?.ok_or_else(|| LclError::DocNotFound(id.clone()))?;
                    if cli.json { print_json(&d)?; } else { outln!("# {}\n\n{}", d.title, d.content); }
                }
                DocCmd::Update { id, title, content, content_file, parent, no_parent } => {
                    let parent = match (parent, no_parent) {
                        (Some(p), _) => Change::Set(p),
                        (None, true) => Change::Clear,
                        (None, false) => Change::Keep,
                    };
                    let d = store.update_doc(&id, title, content.or(content_file), parent)?;
                    if cli.json { print_json(&d)?; } else { outln!("Updated {}", id); }
                }
                DocCmd::Delete { id } => {
                    store.delete_doc(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": id}))?; } else { outln!("Deleted {}", id); }
                }
            }
        },
        Commands::Label { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                LabelCmd::Create { name, color } => {
                    let l = Label { id: uuid::Uuid::new_v4().to_string(), name, color };
                    store.create_label(&l)?;
                    if cli.json { print_json(&l)?; } else { outln!("Created label {}", l.name); }
                }
                LabelCmd::List => {
                    let ls = store.list_labels()?;
                    if cli.json { print_json(&ls)?; } else { for l in ls { outln!("{} {} ({})", l.id, l.name, l.color); } }
                }
                LabelCmd::Delete { name_or_id } => {
                    let (label, issues) = store.delete_label(&name_or_id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": label.name, "id": label.id, "removed_from_issues": issues}))?; }
                    else { outln!("Deleted label {} (removed from {} issue{})", label.name, issues, if issues == 1 { "" } else { "s" }); }
                }
            }
        },
        Commands::Cycle { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                CycleCmd::Create { name, start, end } => {
                    let c = store.create_cycle(Cycle { id: String::new(), name, starts_at: start.map(|d| d.to_string()), ends_at: end.map(|d| d.to_string()), created_at: 0 })?;
                    if cli.json { print_json(&c)?; } else { outln!("Created cycle {}", c.id); }
                }
                CycleCmd::List => {
                    let cs = store.list_cycles()?;
                    if cli.json { print_json(&cs)?; }
                    else {
                        for c in cs {
                            let (start, end) = (c.starts_at.as_deref().unwrap_or("?"), c.ends_at.as_deref().unwrap_or("?"));
                            outln!("{}\t{}\t{start}..{end}", c.id, c.name);
                        }
                    }
                }
            }
        },
        Commands::Search { words, limit } => {
            let query = search_query(&words);
            let store = resolve_store(&cli.workspace)?;
            let found = store.search(&query, limit as usize)?;
            if cli.json { print_json(&found)?; }
            else {
                for h in &found.issues {
                    let fields: Vec<&str> = h.matched_in.iter().map(|f| f.as_str()).collect();
                    outln!("{}\t[{}]\t{}\t{}", h.issue.identifier, h.issue.status, h.issue.title, fields.join(", "));
                }
                for d in &found.docs { outln!("doc\t{}\t{}", d.id, d.title); }
                if found.issue_total > found.issues.len() { outln!("showing {} of {} issues", found.issues.len(), found.issue_total); }
                if found.doc_total > found.docs.len() { outln!("showing {} of {} docs", found.docs.len(), found.doc_total); }
                if found.issue_total == 0 && found.doc_total == 0 { outln!("no matches"); }
            }
        },
        Commands::Export { output, force } => {
            let store = resolve_store(&cli.workspace)?;
            let export = store.export()?;
            match output.as_deref() {
                None | Some("-") => {
                    let mut out = std::io::stdout().lock();
                    out.write_all(export.text.as_bytes()).and_then(|()| out.flush()).map_err(|e| stdout_failed(e, "can't write the export to stdout"))?;
                }
                Some(path) => {
                    write_export(path, &export.text, force)?;
                    if cli.json {
                        let mut v = serde_json::to_value(export.counts)?;
                        v["path"] = serde_json::Value::String(path.to_string());
                        print_json(&v)?;
                    } else {
                        outln!("Exported {} to {path}", export.counts.phrase());
                    }
                }
            }
        }
        Commands::Import { input, dry_run } => {
            let mut store = resolve_store(&cli.workspace)?;
            let s = store.import(&input.0, dry_run)?;
            if cli.json { print_json(&s)?; }
            else {
                let next = identifier(&s.key, s.next_issue_number);
                let what = format!("{} into workspace {} (key {}); next number {next}", s.counts.phrase(), s.workspace, s.key);
                if s.dry_run { outln!("Dry run, nothing written: would import {what}"); } else { outln!("Imported {what}"); }
            }
        }
        Commands::Tui => {
            lcl_tui::run(resolve_store(&cli.workspace)?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every subcommand without an about line, and every argument without
    /// help text, as `lcl ... ARG`. clap's own --help, --version and help
    /// subcommand are left out.
    fn missing_help(cmd: &clap::Command, path: &str, missing: &mut Vec<String>) {
        for arg in cmd.get_arguments() {
            let id = arg.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            if arg.get_help().is_none() && arg.get_long_help().is_none() {
                missing.push(format!("{path} {id}"));
            }
        }
        for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            let sub_path = format!("{path} {}", sub.get_name());
            if sub.get_about().is_none() && sub.get_long_about().is_none() {
                missing.push(format!("{sub_path} (the subcommand)"));
            }
            missing_help(sub, &sub_path, missing);
        }
    }

    #[test]
    fn every_subcommand_and_argument_has_help() {
        let mut cmd = Cli::command();
        cmd.build();
        let mut missing = Vec::new();
        missing_help(&cmd, "lcl", &mut missing);
        assert!(missing.is_empty(), "without help:\n{}", missing.join("\n"));
    }
}
