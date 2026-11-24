use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::parser::ValueSource;
use clap::{ArgGroup, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use lcl_core::db::{
    AssigneeFilter, Change, CycleUpdate, IssueFilter, IssueSort, IssueUpdate, LabelUpdate,
    MilestoneUpdate, NewIssue, Page, Placement, ProjectFilter, ProjectUpdate, Store,
};
use lcl_core::error::{ErrorKind, LclError};
use lcl_core::models::*;
use lcl_core::parse;
use lcl_core::registry::{self, Registry, WorkspaceKey};
use lcl_core::search::SearchQuery;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// `println!` that returns the error when stdout fails (`?`), as
/// StdoutClosed when its reader went away, instead of panicking.
macro_rules! outln {
    ($($arg:tt)*) => {{
        let stdout = std::io::stdout();
        if stdout.is_terminal() {
            writeln!(&stdout, "{}", for_terminal(&format!($($arg)*), true))
        } else {
            writeln!(&stdout, $($arg)*)
        }
        .map_err(|e| stdout_failed(e, "can't write to stdout"))?
    }};
}

#[derive(Parser)]
#[command(
    name = "lcl",
    // Usage and errors say `lcl`, not argv[0]'s name, which on Windows is
    // `lcl.exe`: the text, `--json`'s too, is the same on every platform.
    bin_name = "lcl",
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
    #[arg(
        short = 'w',
        long,
        global = true,
        value_name = "NAME",
        env = "LCL_WORKSPACE"
    )]
    workspace: Option<String>,
    /// The old name of --workspace: refused with a hint. Not global, so only
    /// before the subcommand; a subcommand may have its own --project.
    #[arg(long, hide = true, value_name = "NAME")]
    project: Option<String>,
    #[command(subcommand)]
    cmd: Commands,
}

// One value per run, parsed once: its size doesn't matter, and boxing
// IssueCmd would only add derefs to every match on it.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Commands {
    /// Make a workspace in the current directory, at .lcl/data.sqlite, and
    /// register it, so --workspace NAME finds it from anywhere.
    ///
    /// Run again with the same name and key, it does nothing. It never
    /// renames or re-keys a workspace, and a name another workspace has
    /// (compared by slug, as workspace create compares them) is refused.
    Init(InitArgs),
    /// Create, list and show workspaces.
    Workspace(WorkspaceArgs),
    /// Projects in this workspace: create, list, show, update, delete.
    Project(ProjectArgs),
    /// Milestones, each in one project for good: create, list, show, update, delete.
    Milestone(MilestoneArgs),
    /// Issues in this workspace: create, list, show, update, delete, restore, purge, relate,
    /// unrelate, comment, comments, history.
    Issue(IssueArgs),
    /// Comments, by id: edit, delete, restore, purge. Add and list them with issue comment and
    /// issue comments.
    Comment(CommentArgs),
    /// Docs in this workspace: create, list, show, update, delete, restore, purge.
    Doc(DocArgs),
    /// Labels in this workspace: create, list, delete, restore, purge.
    Label(LabelArgs),
    /// Cycles in this workspace: create, list, update, delete.
    Cycle(CycleArgs),
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
    Search(SearchArgs),
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
    Export(ExportArgs),
    /// Copy this workspace's file to PATH, safely while lcl is in use.
    ///
    /// The copy is one committed state of the workspace: a write that is
    /// under way is left out whole, and is not held up. It is written under
    /// a temporary name in PATH's directory and given its name only when it
    /// is complete and has passed SQLite's integrity check, so PATH is never
    /// half a file. A PATH that exists is refused and left as it is.
    ///
    /// The copy is a workspace file: to restore it, put it where the
    /// workspace's file is while no lcl is running (lcl workspace show
    /// prints that path).
    Backup(BackupArgs),
    /// Restore a file written by lcl export (or a converter) into this
    /// workspace, which must be empty and have the file's key.
    ///
    /// Numbers, refs, creators, comment authors and timestamps are kept as
    /// the file gives them; no status rules run. Every value must pass the
    /// rules the commands apply, and the file is checked whole before
    /// anything is written: a problem exits 2 naming its line and field,
    /// and the workspace is left as it was. The import is one transaction.
    Import(ImportArgs),
    /// Open this workspace in a terminal UI.
    Tui,
    /// Keep this workspace in step with a sync service: send this
    /// machine's changes, receive the others', and top up its issue
    /// numbers.
    ///
    /// Opt in first with lcl sync start URL. The token for the service is
    /// read from LCL_SYNC_TOKEN, and never stored. Only the sync commands
    /// reach the network; a workspace that never opted in makes no call.
    /// With the service out of reach, the file still opens and takes edits:
    /// they go at the next lcl sync. See docs/sync-protocol.md.
    Sync(SyncArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Its name; the current directory's name if left out.
    #[arg(value_parser = parse_workspace_name)]
    name: Option<String>,
    /// The prefix of its issue identifiers, as DRM in DRM-12: a letter,
    /// then up to 9 letters or digits. Uppercased.
    #[arg(long, value_name = "KEY")]
    key: WorkspaceKey,
}

#[derive(Args)]
struct WorkspaceArgs {
    #[command(subcommand)]
    cmd: WorkspaceCmd,
}

#[derive(Args)]
struct ProjectArgs {
    #[command(subcommand)]
    cmd: ProjectCmd,
}

#[derive(Args)]
struct MilestoneArgs {
    #[command(subcommand)]
    cmd: MilestoneCmd,
}

#[derive(Args)]
struct IssueArgs {
    #[command(subcommand)]
    cmd: IssueCmd,
}

#[derive(Args)]
struct CommentArgs {
    #[command(subcommand)]
    cmd: CommentCmd,
}

#[derive(Args)]
struct DocArgs {
    #[command(subcommand)]
    cmd: DocCmd,
}

#[derive(Args)]
struct LabelArgs {
    #[command(subcommand)]
    cmd: LabelCmd,
}

#[derive(Args)]
struct CycleArgs {
    #[command(subcommand)]
    cmd: CycleCmd,
}

#[derive(Args)]
struct SearchArgs {
    /// The words to search for.
    #[arg(value_name = "WORD", required = true, num_args = 1..)]
    words: Vec<String>,
    /// Show at most N issues and N docs.
    #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
    limit: u32,
}

#[derive(Args)]
struct ExportArgs {
    /// Write to PATH (through a temporary file in its directory, renamed
    /// into place) instead of stdout. `-` is stdout.
    #[arg(long, value_name = "PATH")]
    output: Option<String>,
    /// Replace PATH if it exists.
    #[arg(long, requires = "output")]
    force: bool,
}

#[derive(Args)]
struct BackupArgs {
    /// Where to write the copy: a file that doesn't exist, in a
    /// directory that does.
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

#[derive(Args)]
struct ImportArgs {
    /// The file, or `-` for stdin.
    #[arg(value_name = "PATH", value_parser = read_import)]
    input: ImportInput,
    /// Check and count everything, then roll back: nothing is written.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct SyncArgs {
    #[command(subcommand)]
    cmd: Option<SyncCmd>,
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Opt this workspace in: the service at URL makes a replica of it from
    /// this machine's whole history, and gives it its first issue numbers.
    ///
    /// From then on this machine numbers the issues it creates from blocks
    /// of 100 the service gives it, offline or not.
    Start(SyncStartArgs),
    /// Join a replica from another machine, into this workspace: it must be
    /// empty and have the replica's key (make one with lcl workspace create
    /// NAME --key KEY). It receives everything, and its own issue numbers.
    Join(SyncJoinArgs),
    /// Forget the sync service. The file keeps everything, the replica is
    /// left as it is, and issues are numbered from the file's counter
    /// again.
    Stop,
    /// Leave the service: write the whole workspace to PATH, as lcl export
    /// --output does, then delete the replica on the service and forget
    /// it, as stop does. The local file keeps everything.
    ///
    /// A replica already gone counts as deleted. In a workspace that
    /// doesn't sync, it writes the export and nothing else.
    Leave(SyncLeaveArgs),
}

#[derive(Args)]
struct SyncStartArgs {
    /// The service's address: https://HOST (http:// only for localhost).
    #[arg(value_name = "URL")]
    url: String,
}

#[derive(Args)]
struct SyncJoinArgs {
    /// The service's address, as lcl sync start was given it.
    #[arg(value_name = "URL")]
    url: String,
    /// The replica's id, as lcl sync start printed it.
    #[arg(value_name = "ID")]
    id: String,
}

#[derive(Args)]
struct SyncLeaveArgs {
    /// Where to write the export (through a temporary file in its
    /// directory, renamed into place).
    #[arg(long, value_name = "PATH")]
    output: String,
    /// Replace PATH if it exists.
    #[arg(long)]
    force: bool,
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
        std::fs::read(path)
            .map(ImportInput)
            .map_err(|e| format!("can't read {path}: {e}"))
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
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".into());
    let tmp = dir.join(format!(".{name}.lcl-export-{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
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
            Err(_) if exists(target) => {
                Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
            }
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
    Create(WorkspaceCreateArgs),
    /// List registered workspaces: name, slug and path. One whose file is
    /// gone is marked missing.
    List,
    /// Show the current workspace: name, key, path, schema version and
    /// creation time.
    Show,
}

#[derive(Args)]
struct WorkspaceCreateArgs {
    /// Its name, trimmed: with a letter or digit, and a slug no other workspace has.
    #[arg(value_parser = parse_workspace_name)]
    name: String,
    /// The prefix of its issue identifiers, as DRM in DRM-12: a letter,
    /// then up to 9 letters or digits. Uppercased.
    #[arg(long, value_name = "KEY")]
    key: WorkspaceKey,
}

#[derive(Subcommand)]
enum IssueCmd {
    // Status, Priority and IssueRef parse through their FromStr impls, and
    // dates, estimates and colors through the parsers below. A bad value is a
    // clap error: exit 2, before any database is opened. Issue arguments allow
    // a leading '-' so that "-3" reaches the IssueRef parser and is rejected
    // there, rather than being taken for a flag.
    /// Create an issue. It gets the next number: a number is never handed out twice.
    Create(IssueCreateArgs),
    /// List issues: 50 at a time unless told otherwise, newest number first.
    ///
    /// Filters narrow the list together. A repeated --status matches any of
    /// them; a repeated --label requires all of them.
    List(IssueListArgs),
    /// Show an issue: its fields, relations, sub-issues and comments.
    Show(IssueShowArgs),
    /// Change an issue: only what the flags name changes.
    Update(IssueUpdateArgs),
    /// Move an issue to the trash. Refused while it has sub-issues that aren't in the trash.
    ///
    /// The issue keeps its comments, labels, cycles and relations, and its
    /// number and ref stay taken. It is left out of every list, search and
    /// count, and a command that names it is refused, until issue restore
    /// brings it back. issue list --trashed shows the trash.
    Delete(IssueDeleteArgs),
    /// Bring an issue back from the trash, as it was when it was deleted.
    ///
    /// Refused for an issue that isn't in the trash, and while its parent
    /// is: restore the parent first.
    Restore(IssueRestoreArgs),
    /// Remove an issue in the trash for good, or with --all every one.
    ///
    /// This is the only command that removes an issue permanently, and it
    /// can't be undone: the issue goes with its comments, its relations, its
    /// label links and its cycle links. Only an issue in the trash can be
    /// purged (issue delete moves one there), and an issue whose sub-issues
    /// are in the trash goes after them, or with them under --all. A purged
    /// issue's number is never used again. Nothing is purged by itself:
    /// the trash keeps what is in it until this command is run.
    #[command(group(ArgGroup::new("purge_what").required(true).args(["id", "all"])))]
    Purge {
        /// The issue in the trash: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)]
        id: Option<IssueRef>,
        /// Empty the trash: purge every issue in it.
        #[arg(long)]
        all: bool,
    },
    /// Relate two issues: A blocks B, A blocked-by B, A related B, or A duplicate-of B.
    ///
    /// A blocked-by B is the same relation as B blocks A, and related goes
    /// both ways; making a relation that exists, in either form, does nothing.
    /// An issue is a duplicate of at most one issue, and duplicate-of sets its
    /// status to Duplicate. A relation that would make a cycle of blocks, or
    /// of duplicates, is refused.
    Relate(IssueRelateArgs),
    /// Remove a relation, given in either form (B blocks A removes A blocked-by B).
    ///
    /// Statuses stay as they are: unrelating duplicate-of leaves A Duplicate.
    Unrelate(IssueUnrelateArgs),
    /// Add a comment: BODY inline, or --body-file PATH. Exactly one.
    // clap would print the group (BODY) before ID; the arguments parse ID first.
    #[command(group(ArgGroup::new("body_source").required(true).args(["body", "body_file"])))]
    #[command(override_usage = "lcl issue comment [OPTIONS] <ID> <BODY|--body-file <PATH>>")]
    Comment {
        /// The issue: KEY-N, #N, N or its UUID.
        #[arg(allow_negative_numbers = true)]
        id: IssueRef,
        /// The comment inline. For long text, use --body-file.
        #[arg(value_parser = inline_body)]
        body: Option<String>,
        /// Read the comment from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_comment)]
        body_file: Option<String>,
        /// Who is writing it. Without it: LCL_AUTHOR, else USER.
        #[arg(long, value_name = "NAME", value_parser = parse_author)]
        author: Option<String>,
    },
    /// List an issue's comments, oldest first.
    Comments(IssueCommentsArgs),
    /// List what changed in an issue, oldest first: each write's time and
    /// author, and every field it changed, from what to what.
    ///
    /// Its fields, labels, cycles, relations and comments. The first entry
    /// is the issue's creation, or its fields when lcl started to record
    /// them: at an import, or for an issue made before lcl kept history, at
    /// the upgrade that started it. A purge takes its history with it.
    History(IssueHistoryArgs),
}

#[derive(Args)]
struct IssueCreateArgs {
    /// The issue's title. Not blank.
    #[arg(long, value_parser = parse_title)]
    title: String,
    /// The description inline. For long text, use --description-file.
    #[arg(long, value_parser = inline_description, conflicts_with = "description_file")]
    description: Option<String>,
    /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_description)]
    description_file: Option<String>,
    /// Backlog, Todo, In Progress, In Review, Done, Canceled or Duplicate; case, spaces, _ and - are ignored.
    #[arg(long, default_value = "Backlog")]
    status: Status,
    /// 0-4, or none, urgent, high, medium or low (0 is none); case is ignored.
    #[arg(long, default_value = "0")]
    priority: Priority,
    /// Who it is assigned to: a name, trimmed, not blank.
    #[arg(long, value_parser = parse_assignee)]
    assignee: Option<String>,
    /// Its parent issue: KEY-N, #N, N or a UUID.
    #[arg(long, allow_negative_numbers = true)]
    parent: Option<IssueRef>,
    /// A label name (ignoring case); repeat for more.
    #[arg(long, value_parser = parse_label_name)]
    label: Vec<String>,
    /// Its estimate: a whole number, 0 or more.
    #[arg(long, allow_negative_numbers = true, value_parser = parse_estimate)]
    estimate: Option<i64>,
    /// Its due date, as YYYY-MM-DD.
    #[arg(long, value_parser = parse_date)]
    due: Option<NaiveDate>,
    /// The project to put it in, by name (ignoring case) or id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name, conflicts_with = "no_project")]
    project: Option<String>,
    /// Put it in no project (the default).
    #[arg(long, conflicts_with = "milestone")]
    no_project: bool,
    /// Its milestone, by name (ignoring case) or id: one in the --project project, which it needs.
    #[arg(long, value_name = "MILESTONE", value_parser = parse_milestone_name, conflicts_with = "no_milestone", requires = "project")]
    milestone: Option<String>,
    /// Give it no milestone (the default).
    #[arg(long)]
    no_milestone: bool,
    /// The cycle to put it in, by name (trimmed, ignoring case) or id.
    #[arg(long, value_name = "CYCLE", value_parser = parse_cycle_name)]
    cycle: Option<String>,
    /// The id it has elsewhere, as BZR-1532 for a Linear issue: on no
    /// other issue (ignoring case), and not shaped like a reference to one
    /// of this workspace's issues (N, #N, its KEY-N, a UUID).
    #[arg(long = "ref", value_name = "REF", value_parser = parse_ref)]
    issue_ref: Option<String>,
    /// Who is creating it. Without it: LCL_AUTHOR, else USER.
    #[arg(long, value_name = "NAME", value_parser = parse_author)]
    author: Option<String>,
}

#[derive(Args)]
struct IssueListArgs {
    /// Only issues with this status; repeat for any of several.
    #[arg(long, value_name = "STATUS")]
    status: Vec<Status>,
    /// Only issues assigned to NAME, ignoring ASCII case; `none` for unassigned issues.
    #[arg(long, value_name = "NAME", value_parser = parse_assignee_filter)]
    assignee: Option<AssigneeFilter>,
    /// Only issues with this label; repeat to require several.
    #[arg(long, value_name = "LABEL", value_parser = parse_label_name)]
    label: Vec<String>,
    /// Only the direct sub-issues of this issue.
    #[arg(long, allow_negative_numbers = true)]
    parent: Option<IssueRef>,
    /// Only issues with a blocker that isn't Done, Canceled or Duplicate.
    #[arg(long, conflicts_with = "unblocked")]
    blocked: bool,
    /// Only issues with no such blocker.
    #[arg(long)]
    unblocked: bool,
    /// Only issues in this project, by name (ignoring case) or id; `none`
    /// (any case) for issues in no project. A project named "none" is
    /// selected by its id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_filter)]
    project: Option<ProjectFilter>,
    /// Only issues with this milestone, by name (ignoring case) or id: in
    /// the --project project if given, else in any project.
    #[arg(long, value_name = "MILESTONE", value_parser = parse_milestone_name)]
    milestone: Option<String>,
    /// Only issues in this cycle, by name (trimmed, ignoring case) or id.
    #[arg(long, value_name = "CYCLE", value_parser = parse_cycle_name)]
    cycle: Option<String>,
    /// number, created or updated (each newest first), or priority (Urgent first, None last).
    #[arg(long, value_name = "ORDER", default_value = "number")]
    sort: IssueSort,
    /// Show at most N issues.
    #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
    limit: u32,
    /// Skip the first N issues.
    #[arg(long, value_name = "N", default_value_t = 0)]
    offset: u32,
    /// Show every matching issue.
    #[arg(long, conflicts_with_all = ["limit", "offset"])]
    all: bool,
    /// List the trash: only the issues issue delete moved there.
    #[arg(long)]
    trashed: bool,
}

#[derive(Args)]
struct IssueShowArgs {
    /// The issue: KEY-N, #N, N or its UUID.
    #[arg(allow_negative_numbers = true)]
    id: IssueRef,
}

#[derive(Args)]
struct IssueUpdateArgs {
    /// The issue: KEY-N, #N, N or its UUID.
    #[arg(allow_negative_numbers = true)]
    id: IssueRef,
    #[command(flatten)]
    fields: IssueUpdateFields,
    #[command(flatten)]
    labels: IssueUpdateLabels,
    #[command(flatten)]
    place: IssueUpdatePlace,
}

#[derive(Args)]
struct IssueUpdateFields {
    /// Its new title. Not blank.
    #[arg(long, value_parser = parse_title)]
    title: Option<String>,
    /// The description inline; "" clears it. For long text, use --description-file.
    #[arg(long, value_parser = inline_description, conflicts_with = "description_file")]
    description: Option<String>,
    /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_description)]
    description_file: Option<String>,
    /// Backlog, Todo, In Progress, In Review, Done, Canceled or Duplicate; case, spaces, _ and - are ignored.
    #[arg(long)]
    status: Option<Status>,
    /// 0-4, or none, urgent, high, medium or low (0 is none); case is ignored.
    #[arg(long)]
    priority: Option<Priority>,
    /// Assign it to this name: trimmed, not blank.
    #[arg(long, value_parser = parse_assignee, conflicts_with = "no_assignee")]
    assignee: Option<String>,
    /// Unassign the issue.
    #[arg(long)]
    no_assignee: bool,
    /// Its estimate: a whole number, 0 or more.
    #[arg(long, allow_negative_numbers = true, value_parser = parse_estimate, conflicts_with = "no_estimate")]
    estimate: Option<i64>,
    /// Clear the estimate.
    #[arg(long)]
    no_estimate: bool,
    /// Its due date, as YYYY-MM-DD.
    #[arg(long, value_parser = parse_date, conflicts_with = "no_due")]
    due: Option<NaiveDate>,
    /// Clear the due date.
    #[arg(long)]
    no_due: bool,
}

#[derive(Args)]
struct IssueUpdateLabels {
    /// Replace all labels with these (names, ignoring case); repeat for more.
    #[arg(long, value_parser = parse_label_name, conflicts_with_all = ["add_label", "remove_label", "clear_labels"])]
    label: Vec<String>,
    /// Add a label, keeping the others; repeatable.
    #[arg(long, value_parser = parse_label_name)]
    add_label: Vec<String>,
    /// Remove a label (a no-op if the issue doesn't have it); repeatable.
    #[arg(long, value_parser = parse_label_name)]
    remove_label: Vec<String>,
    /// Remove every label.
    #[arg(long, conflicts_with_all = ["add_label", "remove_label"])]
    clear_labels: bool,
    /// Move the issue under this one.
    #[arg(long, allow_negative_numbers = true, conflicts_with = "no_parent")]
    parent: Option<IssueRef>,
    /// Make the issue top-level.
    #[arg(long)]
    no_parent: bool,
}

#[derive(Args)]
struct IssueUpdatePlace {
    /// Move it to this project, by name (ignoring case) or id. A milestone
    /// it has in another project is cleared, unless --milestone is given.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name, conflicts_with = "no_project")]
    project: Option<String>,
    /// Take it out of its project, and so off its milestone.
    #[arg(long, conflicts_with = "milestone")]
    no_project: bool,
    /// Its milestone, by name (ignoring case) or id: one in the issue's
    /// project (the --project one, or the one it is in).
    #[arg(long, value_name = "MILESTONE", value_parser = parse_milestone_name, conflicts_with = "no_milestone")]
    milestone: Option<String>,
    /// Take it off its milestone, leaving its project.
    #[arg(long)]
    no_milestone: bool,
    /// Put it in this cycle, by name (trimmed, ignoring case) or id, and in no other.
    #[arg(long, value_name = "CYCLE", value_parser = parse_cycle_name, conflicts_with = "no_cycle")]
    cycle: Option<String>,
    /// Take it out of every cycle.
    #[arg(long)]
    no_cycle: bool,
    /// Set the id it has elsewhere, as BZR-1532 (see issue create --ref).
    #[arg(long = "ref", value_name = "REF", value_parser = parse_ref, conflicts_with = "no_ref")]
    issue_ref: Option<String>,
    /// Remove its ref.
    #[arg(long)]
    no_ref: bool,
}

#[derive(Args)]
struct IssueDeleteArgs {
    /// The issue: KEY-N, #N, N or its UUID.
    #[arg(allow_negative_numbers = true)]
    id: IssueRef,
}

#[derive(Args)]
struct IssueRestoreArgs {
    /// The issue: KEY-N, #N, N or its UUID.
    #[arg(allow_negative_numbers = true)]
    id: IssueRef,
}

#[derive(Args)]
struct IssueRelateArgs {
    /// The first issue, A: KEY-N, #N, N or a UUID.
    #[arg(allow_negative_numbers = true)]
    a: IssueRef,
    /// blocks, blocked-by, related or duplicate-of.
    kind: RelationKind,
    /// The second issue, B: KEY-N, #N, N or a UUID.
    #[arg(allow_negative_numbers = true)]
    b: IssueRef,
}

#[derive(Args)]
struct IssueUnrelateArgs {
    /// The first issue, A: KEY-N, #N, N or a UUID.
    #[arg(allow_negative_numbers = true)]
    a: IssueRef,
    /// blocks, blocked-by, related or duplicate-of.
    kind: RelationKind,
    /// The second issue, B: KEY-N, #N, N or a UUID.
    #[arg(allow_negative_numbers = true)]
    b: IssueRef,
}

#[derive(Args)]
struct IssueCommentsArgs {
    /// The issue: KEY-N, #N, N or its UUID.
    #[arg(allow_negative_numbers = true)]
    id: IssueRef,
    /// List the issue's comments that are in the trash, and no others.
    #[arg(long)]
    trashed: bool,
}

#[derive(Args)]
struct IssueHistoryArgs {
    /// The issue: KEY-N, #N, N or its UUID. One in the trash has a
    /// history too.
    #[arg(allow_negative_numbers = true)]
    id: IssueRef,
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Create a project in this workspace.
    ///
    /// (lcl 0.1.0's project create --global made a workspace; that is now
    /// lcl workspace create.)
    Create(ProjectCreateArgs),
    /// List projects, by name.
    List(ProjectListArgs),
    /// Show a project: its fields, its issues counted by status, and its
    /// milestones by sort order, then name.
    Show(ProjectShowArgs),
    /// Change a project's name, description, status or target date.
    Update(ProjectUpdateArgs),
    /// Delete a project. Refused while it has issues or milestones.
    Delete(ProjectDeleteArgs),
}

#[derive(Args)]
struct ProjectCreateArgs {
    /// The project's name: unique in this workspace, ignoring case.
    #[arg(value_name = "NAME", value_parser = parse_project_name)]
    name: String,
    /// The description inline. For long text, use --description-file.
    #[arg(long, value_parser = inline_description, conflicts_with = "description_file")]
    description: Option<String>,
    /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_description)]
    description_file: Option<String>,
    /// Backlog, Planned, Started (or In Progress), Paused, Completed or Canceled; case and spaces are ignored.
    #[arg(long, default_value = "Backlog")]
    status: ProjectStatus,
    /// When it should be done, as YYYY-MM-DD.
    #[arg(long, value_name = "DATE", value_parser = parse_date)]
    target_date: Option<NaiveDate>,
    /// lcl 0.1.0's flag for a global workspace: refused with a hint.
    #[arg(long, hide = true)]
    global: bool,
    /// lcl 0.1.0's workspace key: refused with a hint.
    #[arg(long, hide = true, num_args = 0..=1, default_missing_value = "", allow_hyphen_values = true)]
    key: Option<String>,
}

#[derive(Args)]
struct ProjectListArgs {
    /// Only projects with this status; repeat for any of several.
    #[arg(long, value_name = "STATUS")]
    status: Vec<ProjectStatus>,
}

#[derive(Args)]
struct ProjectShowArgs {
    /// The project's name (ignoring case) or id.
    #[arg(value_name = "PROJECT", value_parser = parse_project_name)]
    project: String,
}

#[derive(Args)]
struct ProjectUpdateArgs {
    /// The project's name (ignoring case) or id.
    #[arg(value_name = "PROJECT", value_parser = parse_project_name)]
    project: String,
    /// Its new name: unique in this workspace, ignoring case (its own name in another case is fine).
    #[arg(long, value_name = "NAME", value_parser = parse_project_name)]
    name: Option<String>,
    /// The description inline; "" clears it. For long text, use --description-file.
    #[arg(long, value_parser = inline_description, conflicts_with = "description_file")]
    description: Option<String>,
    /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_description)]
    description_file: Option<String>,
    /// Backlog, Planned, Started (or In Progress), Paused, Completed or Canceled; case and spaces are ignored.
    #[arg(long)]
    status: Option<ProjectStatus>,
    /// When it should be done, as YYYY-MM-DD.
    #[arg(long, value_name = "DATE", value_parser = parse_date, conflicts_with = "no_target_date")]
    target_date: Option<NaiveDate>,
    /// Clear the target date.
    #[arg(long)]
    no_target_date: bool,
}

#[derive(Args)]
struct ProjectDeleteArgs {
    /// The project's name (ignoring case) or id.
    #[arg(value_name = "PROJECT", value_parser = parse_project_name)]
    project: String,
}

#[derive(Subcommand)]
enum MilestoneCmd {
    /// Create a milestone in a project. It stays in that project.
    Create(MilestoneCreateArgs),
    /// List milestones, by project name, then sort order, then name.
    List(MilestoneListArgs),
    /// Show a milestone and its issues counted by status.
    Show(MilestoneShowArgs),
    /// Change a milestone's name, description, target date or sort order. It can't move to another project.
    Update(MilestoneUpdateArgs),
    /// Delete a milestone. Refused while issues have it.
    Delete(MilestoneDeleteArgs),
}

#[derive(Args)]
struct MilestoneCreateArgs {
    /// The milestone's name: unique in its project, ignoring case.
    #[arg(value_name = "NAME", value_parser = parse_milestone_name)]
    name: String,
    /// Its project, by name (ignoring case) or id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)]
    project: String,
    /// The description inline. For long text, use --description-file.
    #[arg(long, value_parser = inline_description, conflicts_with = "description_file")]
    description: Option<String>,
    /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_description)]
    description_file: Option<String>,
    /// When it should be reached, as YYYY-MM-DD.
    #[arg(long, value_name = "DATE", value_parser = parse_date)]
    target_date: Option<NaiveDate>,
    /// Its place among its project's milestones: lower first, ties by name. May be negative.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 0,
        allow_negative_numbers = true
    )]
    sort_order: i64,
}

#[derive(Args)]
struct MilestoneListArgs {
    /// Only this project's milestones, by name (ignoring case) or id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)]
    project: Option<String>,
}

#[derive(Args)]
struct MilestoneShowArgs {
    /// The milestone's name (ignoring case) or id. A name several projects have needs --project.
    #[arg(value_name = "MILESTONE", value_parser = parse_milestone_name)]
    milestone: String,
    /// The project to look in, by name (ignoring case) or id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)]
    project: Option<String>,
}

#[derive(Args)]
struct MilestoneUpdateArgs {
    /// The milestone's name (ignoring case) or id. A name several projects have needs --project.
    #[arg(value_name = "MILESTONE", value_parser = parse_milestone_name)]
    milestone: String,
    /// The project to look in, by name (ignoring case) or id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)]
    project: Option<String>,
    /// Its new name: unique in its project, ignoring case (its own name in another case is fine).
    #[arg(long, value_name = "NAME", value_parser = parse_milestone_name)]
    name: Option<String>,
    /// The description inline; "" clears it. For long text, use --description-file.
    #[arg(long, value_parser = inline_description, conflicts_with = "description_file")]
    description: Option<String>,
    /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_description)]
    description_file: Option<String>,
    /// When it should be reached, as YYYY-MM-DD.
    #[arg(long, value_name = "DATE", value_parser = parse_date, conflicts_with = "no_target_date")]
    target_date: Option<NaiveDate>,
    /// Clear the target date.
    #[arg(long)]
    no_target_date: bool,
    /// Its place among its project's milestones: lower first, ties by name. May be negative.
    #[arg(long, value_name = "N", allow_negative_numbers = true)]
    sort_order: Option<i64>,
}

#[derive(Args)]
struct MilestoneDeleteArgs {
    /// The milestone's name (ignoring case) or id. A name several projects have needs --project.
    #[arg(value_name = "MILESTONE", value_parser = parse_milestone_name)]
    milestone: String,
    /// The project to look in, by name (ignoring case) or id.
    #[arg(long, value_name = "PROJECT", value_parser = parse_project_name)]
    project: Option<String>,
}

#[derive(Subcommand)]
enum DocCmd {
    /// Create a doc, at the top level or under another doc.
    Create(DocCreateArgs),
    /// List docs: id and title, most recently updated first.
    List(DocListArgs),
    /// Show a doc: its title and content.
    Show(DocShowArgs),
    /// Change a doc's title, content or parent: only what the flags name changes.
    Update(DocUpdateArgs),
    /// Move a doc to the trash. Refused while docs that aren't in the trash are under it.
    ///
    /// The doc is left out of doc list and search, and a command that
    /// names it is refused, until doc restore brings it back. doc list
    /// --trashed shows the trash.
    Delete(DocDeleteArgs),
    /// Bring a doc back from the trash, as it was. Refused while its parent is there.
    Restore(DocRestoreArgs),
    /// Remove a doc in the trash for good, or with --all every one. It can't be undone.
    ///
    /// Only a doc in the trash can be purged, and one with docs under it
    /// goes after them, or with them under --all. Nothing is purged by itself.
    #[command(group(ArgGroup::new("purge_what").required(true).args(["id", "all"])))]
    Purge {
        /// The doc's id, as doc list --trashed shows it.
        id: Option<String>,
        /// Purge every doc in the trash.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Args)]
struct DocCreateArgs {
    /// The doc's title. Not blank.
    #[arg(long, value_parser = parse_title)]
    title: String,
    /// The content inline. For long text, use --content-file.
    #[arg(long, value_parser = inline_content, conflicts_with = "content_file")]
    content: Option<String>,
    /// Read the content from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_content)]
    content_file: Option<String>,
    /// The parent doc's id: it must exist.
    #[arg(long, value_name = "ID")]
    parent: Option<String>,
}

#[derive(Args)]
struct DocListArgs {
    /// List the trash: only the docs doc delete moved there.
    #[arg(long)]
    trashed: bool,
}

#[derive(Args)]
struct DocShowArgs {
    /// The doc's id, as doc list shows it.
    id: String,
}

#[derive(Args)]
struct DocUpdateArgs {
    /// The doc's id, as doc list shows it.
    id: String,
    /// Its new title. Not blank.
    #[arg(long, value_parser = parse_title)]
    title: Option<String>,
    /// The content inline; "" clears it. For long text, use --content-file.
    #[arg(long, value_parser = inline_content, conflicts_with = "content_file")]
    content: Option<String>,
    /// Read the content from PATH, or from stdin if PATH is `-`. Stored byte for byte.
    #[arg(long, value_name = "PATH", value_parser = read_content)]
    content_file: Option<String>,
    /// Move it under the doc with this id, which must exist and must not
    /// be this doc or one below it.
    #[arg(long, value_name = "ID", conflicts_with = "no_parent")]
    parent: Option<String>,
    /// Make it a top-level doc.
    #[arg(long)]
    no_parent: bool,
}

#[derive(Args)]
struct DocDeleteArgs {
    /// The doc's id, as doc list shows it.
    id: String,
}

#[derive(Args)]
struct DocRestoreArgs {
    /// The doc's id, as doc list --trashed shows it.
    id: String,
}

#[derive(Subcommand)]
enum LabelCmd {
    /// Create a label.
    Create(LabelCreateArgs),
    /// List labels: id, name and color, by name.
    List(LabelListArgs),
    /// Move a label to the trash, by name (ignoring case) or id. No issue shows it any more.
    ///
    /// Its name stays taken, and the issues that had it are remembered:
    /// label restore puts it back on every one of them.
    Delete(LabelDeleteArgs),
    /// Bring a label back from the trash, onto every issue that had it.
    Restore(LabelRestoreArgs),
    /// Remove a label in the trash for good, or with --all every one. It can't be undone.
    ///
    /// Only a label in the trash can be purged. It goes with its links to
    /// issues, and its name is free again. Nothing is purged by itself.
    #[command(group(ArgGroup::new("purge_what").required(true).args(["name_or_id", "all"])))]
    Purge {
        /// The label's name (ignoring case) or id.
        #[arg(value_parser = parse_label_name)]
        name_or_id: Option<String>,
        /// Purge every label in the trash.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Args)]
struct LabelCreateArgs {
    /// The label's name, trimmed: not blank, and unique ignoring case.
    #[arg(value_parser = parse_label_name)]
    name: String,
    /// Its color, as #rgb or #rrggbb.
    #[arg(long, default_value = "#888888", value_parser = parse_color)]
    color: String,
}

#[derive(Args)]
struct LabelListArgs {
    /// List the trash: only the labels label delete moved there.
    #[arg(long)]
    trashed: bool,
}

#[derive(Args)]
struct LabelDeleteArgs {
    /// The label's name (ignoring case) or id.
    #[arg(value_parser = parse_label_name)]
    name_or_id: String,
}

#[derive(Args)]
struct LabelRestoreArgs {
    /// The label's name (ignoring case) or id.
    #[arg(value_parser = parse_label_name)]
    name_or_id: String,
}

#[derive(Subcommand)]
enum CycleCmd {
    /// Create a cycle, with or without its dates.
    Create(CycleCreateArgs),
    /// List cycles: id, name and dates, newest first.
    List,
    /// Rename a cycle, or set or clear its dates. The end can't come before the start.
    Update(CycleUpdateArgs),
    /// Delete a cycle. Refused while any issue is in it.
    Delete(CycleDeleteArgs),
}

#[derive(Args)]
struct CycleCreateArgs {
    /// The cycle's name, trimmed: no other cycle's, ignoring case.
    #[arg(value_parser = parse_cycle_name)]
    name: String,
    /// Its first day, as YYYY-MM-DD.
    #[arg(long, value_parser = parse_date)]
    start: Option<NaiveDate>,
    /// Its last day, as YYYY-MM-DD: not before --start.
    #[arg(long, value_parser = parse_date)]
    end: Option<NaiveDate>,
}

#[derive(Args)]
struct CycleUpdateArgs {
    /// The cycle's name (trimmed, ignoring case) or id. A name several cycles have needs the id.
    #[arg(value_name = "CYCLE", value_parser = parse_cycle_name)]
    cycle: String,
    /// Its new name, trimmed: no other cycle's, ignoring case (its own in another case is fine).
    #[arg(long, value_name = "NAME", value_parser = parse_cycle_name)]
    name: Option<String>,
    /// Its first day, as YYYY-MM-DD.
    #[arg(long, value_name = "DATE", value_parser = parse_date, conflicts_with = "no_start")]
    start: Option<NaiveDate>,
    /// Clear its first day.
    #[arg(long)]
    no_start: bool,
    /// Its last day, as YYYY-MM-DD: not before its start.
    #[arg(long, value_name = "DATE", value_parser = parse_date, conflicts_with = "no_end")]
    end: Option<NaiveDate>,
    /// Clear its last day.
    #[arg(long)]
    no_end: bool,
}

#[derive(Args)]
struct CycleDeleteArgs {
    /// The cycle's name (trimmed, ignoring case) or id. A name several cycles have needs the id.
    #[arg(value_name = "CYCLE", value_parser = parse_cycle_name)]
    cycle: String,
}

#[derive(Subcommand)]
enum CommentCmd {
    /// Replace a comment's text: BODY inline, or --body-file PATH. Exactly one. Its author and date stay.
    // As issue comment: ID comes first.
    #[command(group(ArgGroup::new("body_source").required(true).args(["body", "body_file"])))]
    #[command(override_usage = "lcl comment edit [OPTIONS] <ID> <BODY|--body-file <PATH>>")]
    Edit {
        /// The comment's id (a UUID, ignoring case), as issue comments --json shows it.
        id: String,
        /// The new text inline. For long text, use --body-file.
        #[arg(value_parser = inline_body)]
        body: Option<String>,
        /// Read the new text from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_comment)]
        body_file: Option<String>,
    },
    /// Move a comment to the trash. issue show, issue comments and search leave it out.
    Delete(CommentDeleteArgs),
    /// Bring a comment back from the trash, as it was.
    Restore(CommentRestoreArgs),
    /// Remove a comment in the trash for good, or with --all every one. It can't be undone.
    ///
    /// Only a comment in the trash can be purged. Nothing is purged by itself.
    #[command(group(ArgGroup::new("purge_what").required(true).args(["id", "all"])))]
    Purge {
        /// The comment's id (a UUID, ignoring case).
        id: Option<String>,
        /// Purge every comment in the trash.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Args)]
struct CommentDeleteArgs {
    /// The comment's id (a UUID, ignoring case), as issue comments --json shows it.
    id: String,
}

#[derive(Args)]
struct CommentRestoreArgs {
    /// The comment's id (a UUID, ignoring case), as issue comments --trashed --json shows it.
    id: String,
}

// The value rules live in lcl_core::parse, shared with `import`; these
// adapt them to clap.

/// A real calendar date written exactly as YYYY-MM-DD.
fn parse_date(s: &str) -> std::result::Result<NaiveDate, String> {
    parse::date(s).map_err(|e| e.0)
}

/// An estimate: a whole number, 0 or more.
fn parse_estimate(s: &str) -> std::result::Result<i64, String> {
    let n: i64 = s
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    if n < 0 {
        return Err(format!("estimate must be 0 or more; got {n}"));
    }
    Ok(n)
}

/// A cycle name or reference, trimmed; blank is refused.
fn parse_title(s: &str) -> std::result::Result<String, String> {
    parse::title(s).map_err(|e| e.0)
}

fn parse_cycle_name(s: &str) -> std::result::Result<String, String> {
    parse::cycle_name(s).map_err(|e| e.0)
}

/// `#rgb` or `#rrggbb`.
fn parse_color(s: &str) -> std::result::Result<String, String> {
    parse::color(s).map_err(|e| e.0)
}

// Long text. Inline text passes through the shell, which splits, expands and
// substitutes it, and through argv, which has a size limit. So text can also
// come from a file or stdin, read here as clap parses: every problem exits 2
// before any database is opened.

/// Inline text, unless it is a bare `-`: whoever writes that means stdin,
/// and storing "-" is never what they want.
fn not_dash(s: &str, file_flag: &str) -> std::result::Result<String, String> {
    if s == "-" {
        Err(format!("to read stdin, use {file_flag} -"))
    } else {
        Ok(s.to_string())
    }
}
fn inline_description(s: &str) -> std::result::Result<String, String> {
    not_dash(s, "--description-file")
}
fn inline_content(s: &str) -> std::result::Result<String, String> {
    not_dash(s, "--content-file")
}
/// A comment inline: not `-`, and not blank (empty or only whitespace).
fn inline_body(s: &str) -> std::result::Result<String, String> {
    if s.trim().is_empty() {
        Err("a comment can't be blank".into())
    } else {
        not_dash(s, "--body-file")
    }
}

fn read_description(path: &str) -> std::result::Result<String, String> {
    read_text(path, "--description-file")
}
fn read_content(path: &str) -> std::result::Result<String, String> {
    read_text(path, "--content-file")
}
/// A comment from a file or stdin: as `read_text`, and not blank. A comment
/// that isn't blank is still stored byte for byte.
fn read_comment(path: &str) -> std::result::Result<String, String> {
    let body = read_text(path, "--body-file")?;
    if body.trim().is_empty() {
        return Err(format!(
            "{} is blank; a comment can't be blank",
            if path == "-" { "stdin" } else { path }
        ));
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
        (
            path,
            std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?,
        )
    };
    String::from_utf8(bytes).map_err(|e| {
        format!(
            "{name} is not valid UTF-8: invalid byte at offset {}",
            e.utf8_error().valid_up_to()
        )
    })
}

/// How long stdin may stay silent before its first byte or EOF:
/// LCL_STDIN_WAIT seconds, default 10. `None` (from 0) waits forever.
fn stdin_wait() -> std::result::Result<Option<Duration>, String> {
    let Ok(v) = std::env::var("LCL_STDIN_WAIT") else {
        return Ok(Some(Duration::from_secs(10)));
    };
    match v.trim().parse::<u64>() {
        Ok(0) => Ok(None),
        Ok(secs) => Ok(Some(Duration::from_secs(secs))),
        Err(_) => Err(format!(
            "LCL_STDIN_WAIT must be a whole number of seconds (0 waits forever), not {v:?}"
        )),
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
        let search = cli
            .find_subcommand_mut("search")
            .expect("the search command");
        search
            .error(
                clap::error::ErrorKind::ValueValidation,
                format!("invalid value '{joined}' for '<WORD>...': {e}"),
            )
            .exit()
    })
}

/// A label name, trimmed; blank is refused.
fn parse_label_name(s: &str) -> std::result::Result<String, String> {
    parse::label_name(s).map_err(|e| e.0)
}

/// An assignee, trimmed; blank is refused. Unassigning is its own flag.
fn parse_assignee(s: &str) -> std::result::Result<String, String> {
    parse::assignee(s).map_err(|e| e.0)
}

/// `issue list --assignee`: a name, trimmed, or `none` (any case) for
/// unassigned issues. Blank is refused.
fn parse_assignee_filter(s: &str) -> std::result::Result<AssigneeFilter, String> {
    let s = s.trim();
    if s.is_empty() {
        Err("an assignee can't be empty; for unassigned issues, use --assignee none".into())
    } else if s.eq_ignore_ascii_case("none") {
        Ok(AssigneeFilter::Unassigned)
    } else {
        Ok(AssigneeFilter::Named(s.to_string()))
    }
}

/// `--ref`: trimmed; at most 100 characters, none of them whitespace or a
/// control character. Blank is refused.
fn parse_ref(s: &str) -> std::result::Result<String, String> {
    parse::issue_ref(s).map_err(|e| e.0)
}

/// `--author`: trimmed; blank is refused (leaving it out means LCL_AUTHOR,
/// else USER).
fn parse_author(s: &str) -> std::result::Result<String, String> {
    parse::author(s).map_err(|e| e.0)
}

/// Who is writing, when --author isn't given: LCL_AUTHOR, else USER,
/// trimmed. Empty (or only whitespace, or not UTF-8) counts as unset; with
/// neither, nobody (null).
fn env_author() -> Option<String> {
    ["LCL_AUTHOR", "USER"].iter().find_map(|v| {
        std::env::var(v)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// A project name or reference, trimmed; blank is refused.
fn parse_project_name(s: &str) -> std::result::Result<String, String> {
    parse::project_name(s).map_err(|e| e.0)
}

/// A milestone name or reference, trimmed; blank is refused.
fn parse_milestone_name(s: &str) -> std::result::Result<String, String> {
    parse::milestone_name(s).map_err(|e| e.0)
}

/// `issue list --project`: a project, or `none` (any case) for issues in no
/// project.
fn parse_project_filter(s: &str) -> std::result::Result<ProjectFilter, String> {
    let s = parse_project_name(s)?;
    if s.eq_ignore_ascii_case("none") {
        Ok(ProjectFilter::NoProject)
    } else {
        Ok(ProjectFilter::Is(s))
    }
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
        let plain = !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._/:@%+=,-".contains(c));
        if plain {
            s.clone()
        } else {
            format!("'{}'", s.replace('\'', r"'\''"))
        }
    };
    args.iter().map(word).collect::<Vec<_>>().join(" ")
}

/// Refuses the commands that were renamed (exit 2, before anything is
/// opened), with the new form.
fn refuse_old_forms(cli: &Cli) {
    let fail = |msg: String| -> ! {
        clap_exit(Cli::command().error(clap::error::ErrorKind::InvalidSubcommand, msg))
    };
    if cli.project.is_some() {
        // The same command line with the first --project (the one before
        // the subcommand; clap took no other) renamed.
        let mut renamed = false;
        let args: Vec<String> = std::env::args()
            .skip(1)
            .map(|a| {
                if renamed {
                    return a;
                }
                if a == "--project" {
                    renamed = true;
                    "--workspace".to_string()
                } else if let Some(v) = a.strip_prefix("--project=") {
                    renamed = true;
                    format!("--workspace={v}")
                } else {
                    a
                }
            })
            .collect();
        fail(format!(
            "the workspace flag is now --workspace: lcl {}",
            shell_line(&args)
        ));
    }
    // lcl 0.1.0's project create made a workspace when given either flag.
    if let Commands::Project(ProjectArgs {
        cmd: ProjectCmd::Create(ProjectCreateArgs { global, key, .. }),
    }) = &cli.cmd
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
    let matches = Cli::command()
        .try_get_matches()
        .unwrap_or_else(|e| clap_exit(e));
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| clap_exit(e));
    if matches.value_source("workspace") == Some(ValueSource::EnvVariable)
        && cli.workspace.as_deref() == Some("")
    {
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
        cli.workspace
            .get_or_insert_with(|| old.to_string_lossy().into_owned());
    }
}

/// Checks that span more than one argument. clap's value parsers have
/// already checked each value on its own; this exits 2 the same way, with
/// the usage of the subcommand at `path` (as clap's own errors have).
fn validate(cli: &Cli) {
    let fail = |path: &[&str], msg: String| -> ! { clap_exit(subcommand_error(path, msg)) };
    if let Commands::Cycle(CycleArgs {
        cmd:
            CycleCmd::Create(CycleCreateArgs {
                start: Some(start),
                end: Some(end),
                ..
            }),
    }) = &cli.cmd
        && end < start
    {
        fail(
            &["cycle", "create"],
            format!("cycle end {end} is before its start {start}"),
        );
    }
    if let Commands::Cycle(CycleArgs {
        cmd:
            CycleCmd::Update(CycleUpdateArgs {
                start: Some(start),
                end: Some(end),
                ..
            }),
    }) = &cli.cmd
        && end < start
    {
        fail(
            &["cycle", "update"],
            format!("cycle end {end} is before its start {start}"),
        );
    }
    // Names match ignoring ASCII case, as label lookups do.
    if let Commands::Issue(IssueArgs {
        cmd:
            IssueCmd::Update(IssueUpdateArgs {
                labels:
                    IssueUpdateLabels {
                        add_label,
                        remove_label,
                        ..
                    },
                ..
            }),
    }) = &cli.cmd
        && let Some(both) = add_label
            .iter()
            .find(|a| remove_label.iter().any(|r| r.eq_ignore_ascii_case(a)))
    {
        fail(
            &["issue", "update"],
            format!("label {both:?} is in both --add-label and --remove-label"),
        );
    }
    if let Commands::Issue(IssueArgs {
        cmd:
            IssueCmd::List(IssueListArgs {
                project: Some(ProjectFilter::NoProject),
                milestone: Some(m),
                ..
            }),
    }) = &cli.cmd
    {
        fail(
            &["issue", "list"],
            LclError::MilestoneInNoProject(m.clone()).to_string(),
        );
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
/// A flag pair as a change: --X V sets, --no-X clears, neither keeps.
fn change<T>(set: Option<T>, clear: bool) -> Change<T> {
    match (set, clear) {
        (Some(v), _) => Change::Set(v),
        (None, true) => Change::Clear,
        (None, false) => Change::Keep,
    }
}

fn placement(
    project: Option<String>,
    no_project: bool,
    milestone: Option<String>,
    no_milestone: bool,
    cycle: Option<String>,
    no_cycle: bool,
) -> Placement {
    Placement {
        project: change(project, no_project),
        milestone: change(milestone, no_milestone),
        cycle: change(cycle, no_cycle),
    }
}

/// Epoch seconds as local time, YYYY-MM-DD HH:MM; the number itself if
/// chrono can't represent it.
fn local_time(at: i64) -> String {
    match chrono::DateTime::from_timestamp(at, 0) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => at.to_string(),
    }
}

/// A comment as people read it: `AUTHOR · DATE` (with ` · ID` when
/// `with_id`), then every line of its body indented two spaces.
fn comment_text(c: &Comment, with_id: bool) -> String {
    let mut out = format!(
        "{} · {}",
        c.author.as_deref().unwrap_or("anon"),
        local_time(c.created_at)
    );
    if with_id {
        out.push_str(&format!(" · {}", c.id));
    }
    for line in c.body.lines() {
        out.push_str(&format!("\n  {line}"));
    }
    out
}

/// An issue's history for people: per write, `DATE · AUTHOR`, then each
/// change indented: `field: OLD → NEW`, `labels: + bug` for a member added
/// and `- bug` for one removed, and for a comment `comment ID: …`. The
/// first entry says `created`, or `first recorded` for an issue lcl began
/// to record after it was made, and lists the fields it set.
fn history_text(history: &[lcl_core::changes::HistoryEntry]) -> String {
    use serde_json::Value;
    const SETS: &[&str] = &[
        "labels",
        "cycles",
        "blocks",
        "blocked_by",
        "related",
        "duplicate_of",
        "duplicates",
    ];
    const TIMES: &[&str] = &[
        "created_at",
        "started_at",
        "completed_at",
        "canceled_at",
        "deleted_at",
    ];
    let shown = |field: &str, v: &Value| -> String {
        match v {
            Value::Null => "none".into(),
            Value::Number(n) if TIMES.contains(&field) => {
                n.as_i64().map_or_else(|| n.to_string(), local_time)
            }
            Value::String(s) => one_line(s),
            other => other.to_string(),
        }
    };
    let mut out = Vec::new();
    for (i, entry) in history.iter().enumerate() {
        let mut head = format!(
            "{} · {}",
            local_time(entry.at),
            entry.author.as_deref().unwrap_or("anon")
        );
        if i == 0 {
            let made = entry
                .changes
                .iter()
                .find(|c| c.field == "created_at" && c.comment_id.is_none())
                .and_then(|c| c.new.as_i64());
            head.push_str(match made {
                Some(t) if (entry.at - t).abs() > 2 => " · first recorded",
                _ => " · created",
            });
        }
        out.push(head);
        for c in &entry.changes {
            let line = match (&c.comment_id, c.field.as_str()) {
                (Some(id), "body") if c.old.is_null() => {
                    format!("comment {id}: + {}", shown("body", &c.new))
                }
                (Some(id), "deleted_at") if c.old.is_null() => {
                    format!("comment {id}: to the trash")
                }
                (Some(id), "deleted_at") => format!("comment {id}: restored"),
                (Some(id), field) => format!(
                    "comment {id}: {field}: {} → {}",
                    shown(field, &c.old),
                    shown(field, &c.new)
                ),
                (None, field) if SETS.contains(&field) => {
                    if c.old.is_null() {
                        format!("{field}: + {}", shown(field, &c.new))
                    } else {
                        format!("{field}: - {}", shown(field, &c.old))
                    }
                }
                (None, field) if i == 0 && c.old.is_null() => {
                    if c.new.as_str() == Some("") {
                        continue;
                    }
                    format!("{field}: {}", shown(field, &c.new))
                }
                (None, field) => format!(
                    "{field}: {} → {}",
                    shown(field, &c.old),
                    shown(field, &c.new)
                ),
            };
            out.push(format!("  {line}"));
        }
    }
    out.join("\n")
}

/// Text on one line for a history: its first line, at most 60 characters,
/// with `…` when anything was left out.
fn one_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    let mut out: String = first.chars().take(60).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// Comments for people: each as `comment_text`, a blank line between them.
fn comments_text(comments: &[Comment], with_id: bool) -> String {
    comments
        .iter()
        .map(|c| comment_text(c, with_id))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// "12 (Backlog 3, Done 9)": the total, then each non-zero status, in
/// Status order.
fn counts_line(c: &IssueCounts) -> String {
    let parts: Vec<String> = c
        .by_status
        .entries()
        .iter()
        .filter(|(_, n)| *n != 0)
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    if parts.is_empty() {
        c.total.to_string()
    } else {
        format!("{} ({})", c.total, parts.join(", "))
    }
}

/// The home directory, where lcl keeps ~/.lcl: $HOME, or on Windows, when
/// HOME is not set, %USERPROFILE% (see `registry::home_dir`). Neither is an
/// error, never a fallback directory; only what needs ~/.lcl asks for it,
/// so a workspace in the current directory works without one.
fn home() -> Result<PathBuf, LclError> {
    registry::home_dir(
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        cfg!(windows),
    )
}

/// An error from the registry under `home`. SQLite's and the file system's
/// own errors don't say which file they are about, so those get the
/// registry's path as context; lcl's other errors name it themselves.
fn in_registry(home: &std::path::Path, e: LclError) -> anyhow::Error {
    let bare = matches!(e, LclError::Db(_) | LclError::Io(_));
    let e = anyhow::Error::new(e);
    if bare {
        e.context(format!(
            "open the workspace registry {}",
            registry::index_path(home).display()
        ))
    } else {
        e
    }
}

/// Opens the registry, creating it if need be.
fn open_registry(home: &std::path::Path) -> Result<Registry> {
    let registry = Registry::open(home).map_err(|e| in_registry(home, e))?;
    if let Some(note) = registry.upgrade_note() {
        eprintln!("lcl: {note}");
    }
    Ok(registry)
}

fn find_local_db() -> Option<PathBuf> {
    let mut cur = std::env::current_dir().ok()?;
    loop {
        let cand = cur.join(".lcl").join("data.sqlite");
        if cand.exists() {
            return Some(cand);
        }
        if !cur.pop() {
            break;
        }
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
            let home = home()?;
            let (path, note) =
                registry::resolve(&home, &slug).map_err(|e| in_registry(&home, e))?;
            if let Some(note) = note {
                eprintln!("lcl: {note}");
            }
            if !path.exists() {
                return Err(LclError::WorkspaceNotFound {
                    name: name.to_string(),
                    path: path.display().to_string(),
                }
                .into());
            }
            path
        }
        None => find_local_db().ok_or(LclError::NoWorkspace)?,
    };
    // Opening an empty file would make a workspace of it, with no name and
    // no key.
    if std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == 0) {
        return Err(LclError::EmptyWorkspaceFile(path.display().to_string()).into());
    }
    let store = open_store(&path).with_context(|| format!("open workspace {}", path.display()))?;
    Ok((path, store))
}

/// Opens a workspace file, and when that upgraded it and the upgrade changed
/// values, says so on stderr. The upgrade runs once, so the line shows once.
///
/// The change log names LCL_AUTHOR, else USER, for its writes; `issue
/// create` and `issue comment` name `--author` instead, when given.
fn open_store(path: &std::path::Path) -> lcl_core::error::Result<Store> {
    let mut store = Store::open(path)?;
    if let Some(note) = store.upgrade_note() {
        eprintln!("lcl: {note}");
    }
    store.set_author(env_author());
    Ok(store)
}

fn resolve_store(workspace: &Option<String>) -> Result<Store> {
    resolve_workspace(workspace.as_deref()).map(|(_, store)| store)
}

/// `2 comments and 1 relation`, zeros included: a purge says what it removed.
fn counted_list(parts: &[(i64, &str, &str)]) -> String {
    let said: Vec<String> = parts
        .iter()
        .map(|(n, one, many)| format!("{n} {}", if *n == 1 { one } else { many }))
        .collect();
    said.join(" and ")
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
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        anyhow::Error::new(StdoutClosed)
    } else {
        anyhow::Error::new(e).context(what)
    }
}

/// Whether errors print as JSON: `--json` is one of the arguments before
/// any `--`. Read from the arguments themselves, so it holds for the errors
/// clap finds before it has parsed anything.
fn json_errors() -> bool {
    std::env::args_os()
        .skip(1)
        .take_while(|a| a != "--")
        .any(|a| a == "--json")
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
    let e = if e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        missing_subcommand().unwrap_or(e)
    } else {
        e
    };
    print_json_error(ErrorKind::Usage, &clap_message(&e));
    std::process::exit(2)
}

/// The error clap gives a group with no subcommand when it doesn't print
/// the group's help instead ("'lcl issue' requires a subcommand but one was
/// not provided", and the subcommands): from parsing the same arguments
/// again with that help turned off.
fn missing_subcommand() -> Option<clap::Error> {
    fn no_help_instead(cmd: clap::Command) -> clap::Command {
        cmd.arg_required_else_help(false)
            .mut_subcommands(no_help_instead)
    }
    no_help_instead(Cli::command()).try_get_matches().err()
}

/// clap's text for an error, without its "error: " prefix, its usage, and
/// its "For more information" line.
fn clap_message(e: &clap::Error) -> String {
    let text = e.render().to_string();
    let kept: Vec<&str> = text
        .split("\n\n")
        .filter(|part| !part.starts_with("Usage:") && !part.starts_with("For more information"))
        .collect();
    let message = kept.join("\n\n");
    let message = message.trim_end();
    message
        .strip_prefix("error: ")
        .unwrap_or(message)
        .to_string()
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
    if e.chain().any(|c| c.is::<std::io::Error>()) {
        ErrorKind::Io
    } else {
        ErrorKind::Other
    }
}

/// An error as lcl prints it: its message, then what caused it, laid out as
/// anyhow lays them out. The causes stop at the first of lcl's own errors,
/// whose message already says what lies beneath it: without that, a SQLite
/// error prints three times over.
fn error_text(e: &anyhow::Error) -> String {
    let mut messages = vec![];
    for cause in e.chain() {
        messages.push(cause.to_string());
        if cause.is::<LclError>() {
            break;
        }
    }
    let mut out = messages.remove(0);
    match messages.as_slice() {
        [] => {}
        [only] => out.push_str(&format!("\n\nCaused by:\n    {only}")),
        several => {
            out.push_str("\n\nCaused by:");
            for (n, message) in several.iter().enumerate() {
                out.push_str(&format!("\n    {n}: {message}"));
            }
        }
    }
    out
}

/// `text` as a terminal may be shown it. Newline and tab stay. Every other
/// C0 control, and DEL, becomes its control picture (ESC is ␛, DEL is ␡);
/// every control outside that set becomes U+FFFD, as U+009B does. Stored
/// text then can't move the cursor, recolour or clear the screen. When
/// `terminal` is false the text is returned unchanged.
fn for_terminal(text: &str, terminal: bool) -> std::borrow::Cow<'_, str> {
    let unsafe_here = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !terminal || !text.contains(unsafe_here) {
        return text.into();
    }
    text.chars()
        .map(|c| match c {
            c if !unsafe_here(c) => c,
            '\u{7f}' => '␡',
            c if (c as u32) < 0x20 => char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'),
            _ => '\u{fffd}',
        })
        .collect::<String>()
        .into()
}

/// Exit 2 for a usage error, as clap does for a bad argument; 1 for anything
/// else. Errors print as `error_text` lays them out, or, under --json, as
/// one JSON line.
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
            let message = error_text(&e);
            if json_errors() {
                print_json_error(kind, &message);
            } else {
                eprintln!(
                    "Error: {}",
                    for_terminal(&message, std::io::stderr().is_terminal())
                );
            }
            ExitCode::from(if kind == ErrorKind::Usage { 2 } else { 1 })
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Commands::Init(InitArgs { name, key }) => {
            // Every check that needs nothing opened comes first, so a
            // refused init creates nothing: not .lcl/, not ~/.lcl.
            let home = home()?;
            let cwd = std::env::current_dir()?;
            let name = match name {
                Some(name) => name,
                None => cwd
                    .file_name()
                    .map(|s| s.to_string_lossy().trim().to_string())
                    .ok_or(LclError::InitNeedsName)?,
            };
            if registry::slug(&name).is_none() {
                return Err(LclError::NoSlug(name).into());
            }
            let dbpath = cwd.join(".lcl").join("data.sqlite");
            let made = open_registry(&home)?.init_workspace(&name, &key, &dbpath, open_store)?;
            if cli.json {
                print_json(&made)?;
            } else if made.created {
                outln!("Initialized workspace '{}' at {}", made.name, made.path);
            } else {
                outln!(
                    "Workspace {:?} (key {}) already exists at {}",
                    made.name,
                    made.key,
                    made.path
                );
            }
        }
        Commands::Project(ProjectArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                ProjectCmd::Create(ProjectCreateArgs {
                    name,
                    description,
                    description_file,
                    status,
                    target_date,
                    global: _,
                    key: _,
                }) => {
                    let description = description.or(description_file).unwrap_or_default();
                    let p = store.create_project(
                        &name,
                        &description,
                        status,
                        target_date.map(|d| d.to_string()).as_deref(),
                    )?;
                    if cli.json {
                        print_json(&p)?;
                    } else {
                        outln!("Created project {:?}", p.name);
                    }
                }
                ProjectCmd::List(ProjectListArgs { status }) => {
                    let all = store.list_projects(&status)?;
                    if cli.json {
                        print_json(&all)?;
                    } else {
                        for p in &all {
                            outln!(
                                "{}\t[{}]\t{}",
                                p.name,
                                p.status,
                                p.target_date.as_deref().unwrap_or("")
                            );
                        }
                    }
                }
                ProjectCmd::Show(ProjectShowArgs { project }) => {
                    let d = store.project_detail(&project)?;
                    if cli.json {
                        print_json(&d)?;
                    } else {
                        let p = &d.project;
                        outln!(
                            "{} [{}]{}",
                            p.name,
                            p.status,
                            p.target_date
                                .as_ref()
                                .map(|t| format!("  target {t}"))
                                .unwrap_or_default()
                        );
                        if !p.description.is_empty() {
                            outln!("\n{}\n", p.description);
                        }
                        outln!("Issues: {}", counts_line(&d.issues));
                        if !d.milestones.is_empty() {
                            outln!("Milestones:");
                            for m in &d.milestones {
                                outln!(
                                    "  {}\t{}\t{} issue{}",
                                    m.name,
                                    m.target_date.as_deref().unwrap_or(""),
                                    m.issues,
                                    if m.issues == 1 { "" } else { "s" }
                                );
                            }
                        }
                    }
                }
                ProjectCmd::Update(ProjectUpdateArgs {
                    project,
                    name,
                    description,
                    description_file,
                    status,
                    target_date,
                    no_target_date,
                }) => {
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
                    if cli.json {
                        print_json(&p)?;
                    } else {
                        outln!("Updated project {:?}", p.name);
                    }
                }
                ProjectCmd::Delete(ProjectDeleteArgs { project }) => {
                    let p = store.delete_project(&project)?;
                    if cli.json {
                        print_json(&serde_json::json!({"deleted": p.name, "id": p.id}))?;
                    } else {
                        outln!("Deleted project {:?}", p.name);
                    }
                }
            }
        }
        Commands::Milestone(MilestoneArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                MilestoneCmd::Create(MilestoneCreateArgs {
                    name,
                    project,
                    description,
                    description_file,
                    target_date,
                    sort_order,
                }) => {
                    let description = description.or(description_file).unwrap_or_default();
                    let target = target_date.map(|d| d.to_string());
                    let m = project_hint(
                        store.create_milestone(
                            &name,
                            &project,
                            &description,
                            target.as_deref(),
                            sort_order,
                        ),
                        Some(&project),
                    )?;
                    if cli.json {
                        print_json(&m)?;
                    } else {
                        outln!("Created milestone {:?} in project {:?}", m.name, m.project);
                    }
                }
                MilestoneCmd::List(MilestoneListArgs { project }) => {
                    let all = project_hint(
                        store.list_milestones(project.as_deref()),
                        project.as_deref(),
                    )?;
                    if cli.json {
                        print_json(&all)?;
                    } else {
                        for m in &all {
                            outln!(
                                "{}\t{}\t{}\t{}",
                                m.project,
                                m.sort_order,
                                m.name,
                                m.target_date.as_deref().unwrap_or("")
                            );
                        }
                    }
                }
                MilestoneCmd::Show(MilestoneShowArgs { milestone, project }) => {
                    let d = project_hint(
                        store.milestone_detail(&milestone, project.as_deref()),
                        project.as_deref(),
                    )?;
                    if cli.json {
                        print_json(&d)?;
                    } else {
                        let m = &d.milestone;
                        outln!(
                            "{} (project {:?}){}",
                            m.name,
                            m.project,
                            m.target_date
                                .as_ref()
                                .map(|t| format!("  target {t}"))
                                .unwrap_or_default()
                        );
                        if !m.description.is_empty() {
                            outln!("\n{}\n", m.description);
                        }
                        outln!("Issues: {}", counts_line(&d.issues));
                    }
                }
                MilestoneCmd::Update(MilestoneUpdateArgs {
                    milestone,
                    project,
                    name,
                    description,
                    description_file,
                    target_date,
                    no_target_date,
                    sort_order,
                }) => {
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
                    let m = project_hint(
                        store.update_milestone(&milestone, project.as_deref(), &u),
                        project.as_deref(),
                    )?;
                    if cli.json {
                        print_json(&m)?;
                    } else {
                        outln!("Updated milestone {:?} in project {:?}", m.name, m.project);
                    }
                }
                MilestoneCmd::Delete(MilestoneDeleteArgs { milestone, project }) => {
                    let m = project_hint(
                        store.delete_milestone(&milestone, project.as_deref()),
                        project.as_deref(),
                    )?;
                    if cli.json {
                        print_json(
                            &serde_json::json!({"deleted": m.name, "id": m.id, "project": m.project}),
                        )?;
                    } else {
                        outln!(
                            "Deleted milestone {:?} from project {:?}",
                            m.name,
                            m.project
                        );
                    }
                }
            }
        }
        Commands::Workspace(WorkspaceArgs { cmd }) => match cmd {
            WorkspaceCmd::Create(WorkspaceCreateArgs { name, key }) => {
                let made = open_registry(&home()?)?.create_workspace(&name, &key)?;
                if cli.json {
                    print_json(&made)?;
                } else {
                    outln!(
                        "Created workspace {:?} (key {}) at {}",
                        made.name,
                        made.key,
                        made.path
                    );
                }
            }
            WorkspaceCmd::List => {
                let all = open_registry(&home()?)?.list()?;
                if cli.json {
                    print_json(&all)?;
                } else {
                    for w in &all {
                        outln!(
                            "{}\t{}\t{}{}",
                            w.name,
                            w.slug,
                            w.path,
                            if w.missing { "\t(missing)" } else { "" }
                        );
                    }
                }
            }
            WorkspaceCmd::Show => {
                let (path, store) = resolve_workspace(cli.workspace.as_deref())?;
                let name = store.get_meta("name")?.unwrap_or_default();
                let key = store.get_meta("key")?.unwrap_or_default();
                // Stored as text; null if absent or not a number.
                let created_at = store
                    .get_meta("created_at")?
                    .and_then(|t| t.parse::<i64>().ok());
                let description = store.get_meta("description")?.unwrap_or_default();
                let schema_version = store.schema_version()?;
                let sync = store.sync_status()?;
                let v = serde_json::json!({"name": name, "key": key, "description": description, "path": path, "schema_version": schema_version, "created_at": created_at, "sync": sync});
                if cli.json {
                    print_json(&v)?;
                } else {
                    outln!("{} [{}] {}", name, key, path.display());
                    if let Some(s) = &sync {
                        outln!(
                            "Syncs with {} as workspace {}; {} issue numbers held",
                            s.server,
                            s.workspace,
                            s.numbers_left
                        );
                    }
                }
            }
        },
        Commands::Issue(IssueArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                IssueCmd::Create(IssueCreateArgs {
                    title,
                    description,
                    description_file,
                    status,
                    priority,
                    assignee,
                    parent,
                    label,
                    estimate,
                    due,
                    project,
                    no_project,
                    milestone,
                    no_milestone,
                    cycle,
                    issue_ref,
                    author,
                }) => {
                    let description = description.or(description_file).unwrap_or_default();
                    let flag = project.clone();
                    let author = author.or_else(env_author);
                    store.set_author(author.clone());
                    let place =
                        placement(project, no_project, milestone, no_milestone, cycle, false);
                    let issue = project_hint(
                        store.create_issue(
                            NewIssue {
                                title,
                                description,
                                status,
                                priority,
                                assignee,
                                parent,
                                r#ref: issue_ref,
                                creator: author,
                                estimate,
                                due_date: due.map(|d| d.to_string()),
                                labels: label,
                            },
                            &place,
                        ),
                        flag.as_deref(),
                    )?;
                    if cli.json {
                        print_json(&issue)?;
                    } else {
                        outln!("Created {}", issue.identifier);
                    }
                }
                IssueCmd::List(IssueListArgs {
                    status,
                    assignee,
                    label,
                    parent,
                    blocked,
                    unblocked,
                    project,
                    milestone,
                    cycle,
                    sort,
                    limit,
                    offset,
                    all,
                    trashed,
                }) => {
                    let page = if all {
                        Page::ALL
                    } else {
                        Page {
                            limit: Some(limit),
                            offset,
                        }
                    };
                    let blocked = if blocked {
                        Some(true)
                    } else if unblocked {
                        Some(false)
                    } else {
                        None
                    };
                    let flag = match &project {
                        Some(ProjectFilter::Is(p)) => Some(p.clone()),
                        _ => None,
                    };
                    let filter = IssueFilter {
                        trashed,
                        statuses: status,
                        assignee,
                        labels: label,
                        parent,
                        blocked,
                        project,
                        milestone,
                        cycle,
                    };
                    let list =
                        project_hint(store.list_issues(&filter, sort, page), flag.as_deref())?;
                    if cli.json {
                        print_json(&list)?;
                    } else {
                        for i in &list.issues {
                            outln!(
                                "{}\t[{}]\t{}\t{}",
                                i.identifier,
                                i.status,
                                i.priority.label(),
                                i.title
                            );
                        }
                        if list.truncated {
                            let first = list.offset as usize + 1;
                            outln!(
                                "showing {first}–{} of {}; use --all, or --limit/--offset",
                                first + list.issues.len() - 1,
                                list.total
                            );
                        }
                    }
                }
                IssueCmd::Show(IssueShowArgs { id }) => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(issue.id.as_str(), false)?;
                    let children = store.children(issue.id.as_str())?;
                    let relations = store.relations(issue.id.as_str())?;
                    if cli.json {
                        let children: Vec<&str> =
                            children.iter().map(|c| c.identifier.as_str()).collect();
                        print_json(
                            &serde_json::json!({"issue": issue, "comments": comments, "children": children, "relations": relations}),
                        )?;
                    } else {
                        outln!("{} {}", issue.identifier, issue.title);
                        outln!(
                            "Status: {}  Priority: {}  Assignee: {}",
                            issue.status,
                            issue.priority.label(),
                            issue.assignee.as_deref().unwrap_or("Unassigned")
                        );
                        if let Some(r) = &issue.r#ref {
                            outln!("Ref: {r}");
                        }
                        if let Some(c) = &issue.creator {
                            outln!("Creator: {c}");
                        }
                        if !issue.labels.is_empty() {
                            outln!("Labels: {}", issue.labels.join(", "));
                        }
                        if let Some(p) = &issue.parent_identifier {
                            outln!("Parent: {p}");
                        }
                        if let Some(p) = &issue.project {
                            outln!("Project: {p}");
                        }
                        if let Some(m) = &issue.milestone {
                            outln!("Milestone: {m}");
                        }
                        let cycles: Vec<&str> =
                            issue.cycles.iter().map(|c| c.name.as_str()).collect();
                        match cycles.as_slice() {
                            [] => {}
                            [one] => outln!("Cycle: {one}"),
                            several => outln!("Cycles: {}", several.join(", ")),
                        }
                        let line = |name: &str, others: &[RelatedIssue]| -> Result<()> {
                            if !others.is_empty() {
                                let others: Vec<String> = others
                                    .iter()
                                    .map(|o| format!("{} [{}]", o.identifier, o.status))
                                    .collect();
                                outln!("{name}: {}", others.join(", "));
                            }
                            Ok(())
                        };
                        line("Blocks", &relations.blocks)?;
                        line("Blocked by", &relations.blocked_by)?;
                        line("Related", &relations.related)?;
                        line("Duplicate of", relations.duplicate_of.as_slice())?;
                        line("Duplicates", &relations.duplicates)?;
                        if !issue.description.is_empty() {
                            outln!("\n{}", issue.description);
                        }
                        if !children.is_empty() {
                            outln!("\nSub-issues:");
                            for c in &children {
                                outln!("  {} {}", c.identifier, c.title);
                            }
                        }
                        if !comments.is_empty() {
                            outln!("\nComments:\n{}", comments_text(&comments, false));
                        }
                    }
                }
                IssueCmd::Update(IssueUpdateArgs {
                    id,
                    fields:
                        IssueUpdateFields {
                            title,
                            description,
                            description_file,
                            status,
                            priority,
                            assignee,
                            no_assignee,
                            estimate,
                            no_estimate,
                            due,
                            no_due,
                        },
                    labels:
                        IssueUpdateLabels {
                            label,
                            add_label,
                            remove_label,
                            clear_labels,
                            parent,
                            no_parent,
                        },
                    place:
                        IssueUpdatePlace {
                            project,
                            no_project,
                            milestone,
                            no_milestone,
                            cycle,
                            no_cycle,
                            issue_ref,
                            no_ref,
                        },
                }) => {
                    let flag = project.clone();
                    let place = placement(
                        project,
                        no_project,
                        milestone,
                        no_milestone,
                        cycle,
                        no_cycle,
                    );
                    let update = IssueUpdate {
                        title,
                        description: description.or(description_file),
                        status,
                        priority,
                        assignee: if no_assignee {
                            Change::Clear
                        } else {
                            match assignee {
                                Some(a) => Change::Set(a),
                                None => Change::Keep,
                            }
                        },
                        parent: if no_parent {
                            Change::Clear
                        } else {
                            match parent {
                                Some(p) => Change::Set(p),
                                None => Change::Keep,
                            }
                        },
                        estimate: if no_estimate {
                            Change::Clear
                        } else {
                            match estimate {
                                Some(e) => Change::Set(e),
                                None => Change::Keep,
                            }
                        },
                        due_date: if no_due {
                            Change::Clear
                        } else {
                            match due {
                                Some(d) => Change::Set(d.to_string()),
                                None => Change::Keep,
                            }
                        },
                        r#ref: if no_ref {
                            Change::Clear
                        } else {
                            match issue_ref {
                                Some(r) => Change::Set(r),
                                None => Change::Keep,
                            }
                        },
                        // Replace, then clear, then remove, then add, inside
                        // the write, against the labels just read. An unknown
                        // name being removed is an error even when the issue
                        // doesn't have it.
                        labels: LabelUpdate {
                            replace: (!label.is_empty()).then_some(label),
                            add: add_label,
                            remove: remove_label,
                            clear: clear_labels,
                        },
                    };
                    let (issue, cleared) =
                        project_hint(store.update_issue(&id, &place, &update), flag.as_deref())?;
                    if let Some(c) = cleared {
                        eprintln!(
                            "lcl: milestone {:?} cleared: it belongs to project {:?}",
                            c.milestone, c.project
                        );
                    }
                    if cli.json {
                        print_json(&issue)?;
                    } else {
                        outln!("Updated {}", issue.identifier);
                    }
                }
                IssueCmd::Delete(IssueDeleteArgs { id }) => {
                    let issue = store.delete_issue(&id)?;
                    if cli.json {
                        print_json(
                            &serde_json::json!({"deleted": issue.identifier, "id": issue.id}),
                        )?;
                    } else {
                        outln!(
                            "Moved {0} to the trash; restore it with: lcl issue restore {0}",
                            issue.identifier
                        );
                    }
                }
                IssueCmd::Restore(IssueRestoreArgs { id }) => {
                    let issue = store.restore_issue(&id)?;
                    if cli.json {
                        print_json(&issue)?;
                    } else {
                        outln!("Restored {}", issue.identifier);
                    }
                }
                IssueCmd::Purge { id, all: _ } => {
                    let purged = match &id {
                        Some(id) => store.purge_issue(id)?,
                        None => store.purge_trash()?,
                    };
                    if cli.json {
                        print_json(&purged)?;
                    } else if purged.issues == 0 {
                        outln!("The trash is empty");
                    } else {
                        outln!(
                            "Purged {} with {}",
                            purged.purged.join(", "),
                            counted_list(&[
                                (purged.comments, "comment", "comments"),
                                (purged.relations, "relation", "relations"),
                            ])
                        );
                    }
                }
                IssueCmd::Relate(IssueRelateArgs { a, kind, b }) => {
                    let (relation, created) = store.relate(&a, kind, &b)?;
                    if cli.json {
                        print_json(
                            &serde_json::json!({"issue": relation.issue, "kind": relation.kind, "related": relation.related, "created": created}),
                        )?;
                    } else if !created {
                        outln!("Already related: {relation}");
                    } else if kind == RelationKind::DuplicateOf {
                        outln!("{relation}; {} is Duplicate", relation.issue);
                    } else {
                        outln!("{relation}");
                    }
                }
                IssueCmd::Unrelate(IssueUnrelateArgs { a, kind, b }) => {
                    let relation = store.unrelate(&a, kind, &b)?;
                    if cli.json {
                        print_json(
                            &serde_json::json!({"issue": relation.issue, "kind": relation.kind, "related": relation.related, "removed": true}),
                        )?;
                    } else {
                        outln!("Removed: {relation}");
                    }
                }
                IssueCmd::Comment {
                    id,
                    body,
                    body_file,
                    author,
                } => {
                    let Some(body) = body.or(body_file) else {
                        unreachable!("clap requires BODY or --body-file")
                    };
                    let author = author.or_else(env_author);
                    store.set_author(author.clone());
                    let c = store.add_comment(&id, body, author)?;
                    if cli.json {
                        print_json(&c)?;
                    } else {
                        outln!("Comment added");
                    }
                }
                IssueCmd::Comments(IssueCommentsArgs { id, trashed }) => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(issue.id.as_str(), trashed)?;
                    if cli.json {
                        print_json(&comments)?;
                    } else if !comments.is_empty() {
                        outln!("{}", comments_text(&comments, true));
                    }
                }
                IssueCmd::History(IssueHistoryArgs { id }) => {
                    let history = store.issue_history(&id)?;
                    if cli.json {
                        print_json(&history)?;
                    } else if !history.is_empty() {
                        outln!("{}", history_text(&history));
                    }
                }
            }
        }
        Commands::Comment(CommentArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                CommentCmd::Edit {
                    id,
                    body,
                    body_file,
                } => {
                    let body = body
                        .or(body_file)
                        .expect("clap requires one of BODY and --body-file");
                    let c = store.edit_comment(&id, body)?;
                    if cli.json {
                        print_json(&c)?;
                    } else {
                        outln!("Comment edited");
                    }
                }
                CommentCmd::Delete(CommentDeleteArgs { id }) => {
                    let (c, issue) = store.delete_comment(&id)?;
                    if cli.json {
                        print_json(&serde_json::json!({"deleted": c.id, "issue": issue}))?;
                    } else {
                        outln!(
                            "Moved comment {0} to the trash; restore it with: lcl comment restore {0}",
                            c.id
                        );
                    }
                }
                CommentCmd::Restore(CommentRestoreArgs { id }) => {
                    let (c, _) = store.restore_comment(&id)?;
                    if cli.json {
                        print_json(&c)?;
                    } else {
                        outln!("Restored comment {}", c.id);
                    }
                }
                CommentCmd::Purge { id, all: _ } => {
                    let purged = match &id {
                        Some(id) => store.purge_comment(id)?,
                        None => store.purge_comments()?,
                    };
                    if cli.json {
                        print_json(&purged)?;
                    } else if purged.comments == 0 {
                        outln!("No comments are in the trash");
                    } else {
                        outln!(
                            "Purged {}",
                            counted_list(&[(purged.comments, "comment", "comments")])
                        );
                    }
                }
            }
        }
        Commands::Doc(DocArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                DocCmd::Create(DocCreateArgs {
                    title,
                    content,
                    content_file,
                    parent,
                }) => {
                    let content = content.or(content_file).unwrap_or_default();
                    let d = store.create_doc(Doc {
                        id: String::new(),
                        title,
                        content,
                        parent_id: parent,
                        created_at: 0,
                        updated_at: 0,
                        deleted_at: None,
                    })?;
                    if cli.json {
                        print_json(&d)?;
                    } else {
                        outln!("Created doc {}", d.id);
                    }
                }
                DocCmd::List(DocListArgs { trashed }) => {
                    let docs = store.list_docs(trashed)?;
                    if cli.json {
                        print_json(&docs)?;
                    } else {
                        for d in docs {
                            outln!("{}\t{}", d.id, d.title);
                        }
                    }
                }
                DocCmd::Show(DocShowArgs { id }) => {
                    let d = store
                        .get_doc(&id)?
                        .ok_or_else(|| LclError::DocNotFound(id.clone()))?;
                    if cli.json {
                        print_json(&d)?;
                    } else {
                        outln!("# {}\n\n{}", d.title, d.content);
                    }
                }
                DocCmd::Update(DocUpdateArgs {
                    id,
                    title,
                    content,
                    content_file,
                    parent,
                    no_parent,
                }) => {
                    let parent = match (parent, no_parent) {
                        (Some(p), _) => Change::Set(p),
                        (None, true) => Change::Clear,
                        (None, false) => Change::Keep,
                    };
                    let d = store.update_doc(&id, title, content.or(content_file), parent)?;
                    if cli.json {
                        print_json(&d)?;
                    } else {
                        outln!("Updated {}", id);
                    }
                }
                DocCmd::Delete(DocDeleteArgs { id }) => {
                    let doc = store.delete_doc(&id)?;
                    if cli.json {
                        print_json(&serde_json::json!({"deleted": doc.id}))?;
                    } else {
                        outln!(
                            "Moved doc {0} to the trash; restore it with: lcl doc restore {0}",
                            doc.id
                        );
                    }
                }
                DocCmd::Restore(DocRestoreArgs { id }) => {
                    let doc = store.restore_doc(&id)?;
                    if cli.json {
                        print_json(&doc)?;
                    } else {
                        outln!("Restored doc {}", doc.id);
                    }
                }
                DocCmd::Purge { id, all: _ } => {
                    let purged = match &id {
                        Some(id) => store.purge_doc(id)?,
                        None => store.purge_docs()?,
                    };
                    if cli.json {
                        print_json(&purged)?;
                    } else if purged.docs == 0 {
                        outln!("No docs are in the trash");
                    } else {
                        outln!("Purged {}", counted_list(&[(purged.docs, "doc", "docs")]));
                    }
                }
            }
        }
        Commands::Label(LabelArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                LabelCmd::Create(LabelCreateArgs { name, color }) => {
                    let l = Label {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        color,
                        deleted_at: None,
                    };
                    store.create_label(&l)?;
                    if cli.json {
                        print_json(&l)?;
                    } else {
                        outln!("Created label {}", l.name);
                    }
                }
                LabelCmd::List(LabelListArgs { trashed }) => {
                    let ls = store.list_labels(trashed)?;
                    if cli.json {
                        print_json(&ls)?;
                    } else {
                        for l in ls {
                            outln!("{} {} ({})", l.id, l.name, l.color);
                        }
                    }
                }
                LabelCmd::Delete(LabelDeleteArgs { name_or_id }) => {
                    let (label, issues) = store.delete_label(&name_or_id)?;
                    if cli.json {
                        print_json(
                            &serde_json::json!({"deleted": label.name, "id": label.id, "removed_from_issues": issues}),
                        )?;
                    } else {
                        outln!(
                            "Moved label {} to the trash (off {}); restore it with: lcl label restore {}",
                            label.name,
                            counted_list(&[(issues, "issue", "issues")]),
                            label.name
                        );
                    }
                }
                LabelCmd::Restore(LabelRestoreArgs { name_or_id }) => {
                    let (label, issues) = store.restore_label(&name_or_id)?;
                    if cli.json {
                        print_json(
                            &serde_json::json!({"restored": label.name, "id": label.id, "restored_to_issues": issues}),
                        )?;
                    } else {
                        outln!(
                            "Restored label {} (back on {})",
                            label.name,
                            counted_list(&[(issues, "issue", "issues")])
                        );
                    }
                }
                LabelCmd::Purge { name_or_id, all: _ } => {
                    let purged = match &name_or_id {
                        Some(name) => store.purge_label(name)?,
                        None => store.purge_labels()?,
                    };
                    if cli.json {
                        print_json(&purged)?;
                    } else if purged.labels == 0 {
                        outln!("No labels are in the trash");
                    } else {
                        outln!(
                            "Purged {}",
                            counted_list(&[(purged.labels, "label", "labels")])
                        );
                    }
                }
            }
        }
        Commands::Cycle(CycleArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                CycleCmd::Create(CycleCreateArgs { name, start, end }) => {
                    let c = store.create_cycle(Cycle {
                        id: String::new(),
                        name,
                        starts_at: start.map(|d| d.to_string()),
                        ends_at: end.map(|d| d.to_string()),
                        created_at: 0,
                    })?;
                    if cli.json {
                        print_json(&c)?;
                    } else {
                        outln!("Created cycle {}", c.id);
                    }
                }
                CycleCmd::List => {
                    let cs = store.list_cycles()?;
                    if cli.json {
                        print_json(&cs)?;
                    } else {
                        for CycleListing { cycle: c, .. } in cs {
                            let (start, end) = (
                                c.starts_at.as_deref().unwrap_or("?"),
                                c.ends_at.as_deref().unwrap_or("?"),
                            );
                            outln!("{}\t{}\t{start}..{end}", c.id, c.name);
                        }
                    }
                }
                CycleCmd::Update(CycleUpdateArgs {
                    cycle,
                    name,
                    start,
                    no_start,
                    end,
                    no_end,
                }) => {
                    let u = CycleUpdate {
                        name,
                        starts_at: change(start.map(|d| d.to_string()), no_start),
                        ends_at: change(end.map(|d| d.to_string()), no_end),
                    };
                    let c = store.update_cycle(&cycle, &u)?;
                    if cli.json {
                        print_json(&c)?;
                    } else {
                        outln!("Updated cycle {:?}", c.name);
                    }
                }
                CycleCmd::Delete(CycleDeleteArgs { cycle }) => {
                    let c = store.delete_cycle(&cycle)?;
                    if cli.json {
                        print_json(&serde_json::json!({"deleted": c.name, "id": c.id}))?;
                    } else {
                        outln!("Deleted cycle {:?}", c.name);
                    }
                }
            }
        }
        Commands::Search(SearchArgs { words, limit }) => {
            let query = search_query(&words);
            let store = resolve_store(&cli.workspace)?;
            let found = store.search(&query, limit as usize)?;
            if cli.json {
                print_json(&found)?;
            } else {
                for h in &found.issues {
                    let fields: Vec<&str> = h.matched_in.iter().map(|f| f.as_str()).collect();
                    outln!(
                        "{}\t[{}]\t{}\t{}",
                        h.issue.identifier,
                        h.issue.status,
                        h.issue.title,
                        fields.join(", ")
                    );
                }
                for d in &found.docs {
                    outln!("doc\t{}\t{}", d.id, d.title);
                }
                if found.issue_total > found.issues.len() {
                    outln!(
                        "showing {} of {} issues",
                        found.issues.len(),
                        found.issue_total
                    );
                }
                if found.doc_total > found.docs.len() {
                    outln!("showing {} of {} docs", found.docs.len(), found.doc_total);
                }
                if found.issue_total == 0 && found.doc_total == 0 {
                    outln!("no matches");
                }
            }
        }
        Commands::Export(ExportArgs { output, force }) => {
            let store = resolve_store(&cli.workspace)?;
            let export = store.export()?;
            match output.as_deref() {
                None | Some("-") => {
                    let mut out = std::io::stdout().lock();
                    out.write_all(export.text.as_bytes())
                        .and_then(|()| out.flush())
                        .map_err(|e| stdout_failed(e, "can't write the export to stdout"))?;
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
        Commands::Sync(SyncArgs { cmd }) => {
            let mut store = resolve_store(&cli.workspace)?;
            let token = || std::env::var(lcl_core::sync::TOKEN_VAR).ok();
            match cmd {
                Some(SyncCmd::Start(SyncStartArgs { url })) => {
                    let service = lcl_core::sync::Http::new(&url, token())?;
                    let s = store.sync_start(&service)?;
                    if cli.json {
                        print_json(&s)?;
                    } else {
                        outln!(
                            "Syncing with {} as workspace {}: sent {}; {} issue numbers held",
                            s.server,
                            s.workspace,
                            counted_list(&[(s.sent as i64, "change", "changes")]),
                            s.numbers_left
                        );
                        outln!(
                            "Join from another machine with: lcl sync join {} {}",
                            s.server,
                            s.workspace
                        );
                    }
                }
                Some(SyncCmd::Join(SyncJoinArgs { url, id })) => {
                    let service = lcl_core::sync::Http::new(&url, token())?;
                    let s = store.sync_join(&service, &id)?;
                    if cli.json {
                        print_json(&s)?;
                    } else {
                        outln!(
                            "Joined workspace {} at {}: received {}; {} issue numbers held",
                            s.workspace,
                            s.server,
                            counted_list(&[(s.received as i64, "change", "changes")]),
                            s.numbers_left
                        );
                    }
                }
                Some(SyncCmd::Leave(SyncLeaveArgs { output, force })) => {
                    // The token is checked first, and the export written
                    // before anything is deleted.
                    let service = match store.sync_status()? {
                        Some(s) => Some(lcl_core::sync::Http::new(&s.server, token())?),
                        None => None,
                    };
                    let export = store.export()?;
                    write_export(&output, &export.text, force)?;
                    let left = match &service {
                        Some(svc) => Some(store.sync_leave(svc)?),
                        None => None,
                    };
                    if cli.json {
                        let mut v = serde_json::to_value(export.counts)?;
                        v["path"] = serde_json::Value::String(output.clone());
                        v["server"] = serde_json::json!(left.as_ref().map(|l| &l.server));
                        v["workspace"] = serde_json::json!(left.as_ref().map(|l| &l.workspace));
                        print_json(&v)?;
                    } else {
                        outln!("Exported {} to {output}", export.counts.phrase());
                        if let Some(l) = &left {
                            outln!(
                                "Deleted the replica {} at {}; this workspace no longer syncs",
                                l.workspace,
                                l.server
                            );
                        }
                    }
                }
                Some(SyncCmd::Stop) => {
                    let s = store.sync_stop()?;
                    if cli.json {
                        print_json(&s)?;
                    } else {
                        outln!(
                            "Stopped syncing with {}; its replica {} is left as it is",
                            s.server,
                            s.workspace
                        );
                    }
                }
                None => {
                    let Some(status) = store.sync_status()? else {
                        return Err(LclError::NotSyncing.into());
                    };
                    let service = lcl_core::sync::Http::new(&status.server, token())?;
                    let r = store.sync(&service)?;
                    if cli.json {
                        print_json(&r)?;
                    } else {
                        outln!(
                            "Sent {}, received {}; {} issue numbers held",
                            counted_list(&[(r.sent as i64, "change", "changes")]),
                            counted_list(&[(r.received as i64, "change", "changes")]),
                            r.numbers_left
                        );
                        let what = |c: &lcl_core::sync::SyncConflict| match &c.identifier {
                            Some(id) if c.kind == "issue" => format!("{id} {}", c.field),
                            Some(id) => format!("{} {} on {id} {}", c.kind, c.record, c.field),
                            None => format!("{} {} {}", c.kind, c.record, c.field),
                        };
                        for c in &r.not_applied {
                            outln!(
                                "Not applied by the service (purged there, or in conflict): {}",
                                what(c)
                            );
                        }
                        for c in &r.overwritten {
                            outln!("Overwritten by a later change from elsewhere: {}", what(c));
                        }
                    }
                }
            }
        }
        Commands::Backup(BackupArgs { path }) => {
            let store = resolve_store(&cli.workspace)?;
            let report = store.backup(&path)?;
            let shown = path.display().to_string();
            if cli.json {
                let mut v = serde_json::to_value(&report)?;
                v["path"] = serde_json::Value::String(shown);
                print_json(&v)?;
            } else {
                outln!(
                    "Backed up to {shown} ({} bytes, schema {})",
                    report.bytes,
                    report.schema_version
                );
            }
        }
        Commands::Import(ImportArgs { input, dry_run }) => {
            let mut store = resolve_store(&cli.workspace)?;
            let s = store.import(&input.0, dry_run)?;
            if cli.json {
                print_json(&s)?;
            } else {
                let next = identifier(&s.key, s.next_issue_number);
                let what = format!(
                    "{} into workspace {} (key {}); next number {next}",
                    s.counts.phrase(),
                    s.workspace,
                    s.key
                );
                if s.dry_run {
                    outln!("Dry run, nothing written: would import {what}");
                } else {
                    outln!("Imported {what}");
                }
                if !s.skipped.is_empty() {
                    let total: usize = s.skipped.iter().map(|k| k.records).sum();
                    let types: Vec<String> = s
                        .skipped
                        .iter()
                        .map(|k| format!("{} ({})", k.kind, k.records))
                        .collect();
                    outln!(
                        "{} {total} record{} of types this lcl doesn't know: {}",
                        if s.dry_run { "Would skip" } else { "Skipped" },
                        if total == 1 { "" } else { "s" },
                        types.join(", ")
                    );
                }
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

    /// Every argument of every subcommand is built on a 512 KiB stack, in a
    /// debug build too (LOCL-46). clap's derive builds a command's arguments
    /// in one function, and an unoptimized build gives each temporary its own
    /// slot, so a command with many arguments has a large frame: before the
    /// big ones got `Args` structs of their own, `lcl --version` needed about
    /// 900 KB of stack, near the 1 MiB a Windows main thread gets.
    #[test]
    fn the_command_builds_on_a_small_stack() {
        // A Linux debug build needs about 370 KB here; main's code before
        // LOCL-46 needed over 768.
        let kind = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                Cli::try_parse_from(["lcl", "--version"])
                    .map(|_| ())
                    .unwrap_err()
                    .kind()
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(kind, clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn a_terminal_is_shown_control_characters_as_pictures() {
        let text = "a\u{1b}[2Jb\u{7}\u{7f}\u{9b}\tc\nd é 日本";
        assert_eq!(for_terminal(text, true), "a␛[2Jb␇␡\u{fffd}\tc\nd é 日本");
        assert_eq!(for_terminal(text, false), text);
        assert!(matches!(
            for_terminal("plain\ttext\n", true),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn an_error_s_causes_stop_at_lcl_s_own() {
        let io = || std::io::Error::other("disk on fire");
        // lcl's error says what is beneath it, so nothing beneath is printed.
        let e = anyhow::Error::new(LclError::Io(io()));
        assert_eq!(error_text(&e), "io error: disk on fire");
        let e = e.context("open workspace W");
        assert_eq!(
            error_text(&e),
            "open workspace W\n\nCaused by:\n    io error: disk on fire"
        );
        let e = e.context("outer");
        assert_eq!(
            error_text(&e),
            "outer\n\nCaused by:\n    0: open workspace W\n    1: io error: disk on fire"
        );
        // Another error's causes are all printed.
        let e = anyhow::Error::new(io()).context("can't write X");
        assert_eq!(
            error_text(&e),
            "can't write X\n\nCaused by:\n    disk on fire"
        );
    }

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
