use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::parser::ValueSource;
use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
use lcl_core::db::{AssigneeFilter, IssueFilter, IssueSort, Page, Store};
use lcl_core::error::LclError;
use lcl_core::models::*;
use lcl_core::registry::{self, Registry, WorkspaceKey};
use lcl_core::search::SearchQuery;
use std::io::{IsTerminal, Read};
use std::time::Duration;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "lcl", version, about = "Local Linear-like tracker (one SQLite database per workspace)")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    /// The global workspace NAME (~/.lcl/projects/<slug>.sqlite). Without it,
    /// the nearest .lcl/data.sqlite at or above the current directory.
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
    /// Make a workspace in the current directory, at .lcl/data.sqlite.
    Init { name: Option<String> },
    /// Create, list and show workspaces.
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// The old name of `workspace`: refused with a hint.
    #[command(hide = true, disable_help_flag = true)]
    Project {
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    Doc {
        #[command(subcommand)]
        cmd: DocCmd,
    },
    Label {
        #[command(subcommand)]
        cmd: LabelCmd,
    },
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
    Tui,
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
    Create {
        #[arg(long)] title: String,
        /// The description inline. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        #[arg(long, default_value = "Backlog")] status: Status,
        #[arg(long, default_value = "0")] priority: Priority,
        #[arg(long, value_parser = parse_assignee)] assignee: Option<String>,
        #[arg(long, allow_negative_numbers = true)] parent: Option<IssueRef>,
        /// A label name (ignoring case); repeat for more.
        #[arg(long, value_parser = parse_label_name)] label: Vec<String>,
        #[arg(long, allow_negative_numbers = true, value_parser = clap::value_parser!(i64).range(0..))] estimate: Option<i64>,
        #[arg(long, value_parser = parse_date)] due: Option<NaiveDate>,
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
        /// number, created or updated (each newest first), or priority (Urgent first, None last).
        #[arg(long, value_name = "ORDER", default_value = "number")] sort: IssueSort,
        /// Show at most N issues.
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))] limit: u32,
        /// Skip the first N issues.
        #[arg(long, value_name = "N", default_value_t = 0)] offset: u32,
        /// Show every matching issue.
        #[arg(long, conflicts_with_all = ["limit", "offset"])] all: bool,
    },
    Show { #[arg(allow_negative_numbers = true)] id: IssueRef },
    Update {
        #[arg(allow_negative_numbers = true)] id: IssueRef,
        #[arg(long)] title: Option<String>,
        /// The description inline; "" clears it. For long text, use --description-file.
        #[arg(long, value_parser = inline_description, conflicts_with = "description_file")] description: Option<String>,
        /// Read the description from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_description)] description_file: Option<String>,
        #[arg(long)] status: Option<Status>,
        #[arg(long)] priority: Option<Priority>,
        #[arg(long, value_parser = parse_assignee, conflicts_with = "no_assignee")] assignee: Option<String>,
        /// Unassign the issue.
        #[arg(long)] no_assignee: bool,
        #[arg(long, allow_negative_numbers = true, value_parser = clap::value_parser!(i64).range(0..), conflicts_with = "no_estimate")] estimate: Option<i64>,
        /// Clear the estimate.
        #[arg(long)] no_estimate: bool,
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
    },
    Delete { #[arg(allow_negative_numbers = true)] id: IssueRef },
    /// Relate two issues: A blocks B, A blocked-by B, A related B, or A duplicate-of B.
    ///
    /// A blocked-by B is the same relation as B blocks A, and related goes
    /// both ways; making a relation that exists, in either form, does nothing.
    /// An issue is a duplicate of at most one issue, and duplicate-of sets its
    /// status to Duplicate. A relation that would make a cycle of blocks, or
    /// of duplicates, is refused.
    Relate {
        #[arg(allow_negative_numbers = true)] a: IssueRef,
        /// blocks, blocked-by, related or duplicate-of.
        kind: RelationKind,
        #[arg(allow_negative_numbers = true)] b: IssueRef,
    },
    /// Remove a relation, given in either form (B blocks A removes A blocked-by B).
    ///
    /// Statuses stay as they are: unrelating duplicate-of leaves A Duplicate.
    Unrelate {
        #[arg(allow_negative_numbers = true)] a: IssueRef,
        /// blocks, blocked-by, related or duplicate-of.
        kind: RelationKind,
        #[arg(allow_negative_numbers = true)] b: IssueRef,
    },
    /// Add a comment: BODY inline, or --body-file PATH. Exactly one.
    #[command(group(ArgGroup::new("body_source").required(true).args(["body", "body_file"])))]
    Comment {
        #[arg(allow_negative_numbers = true)] id: IssueRef,
        /// The comment inline. For long text, use --body-file.
        #[arg(value_parser = inline_body)] body: Option<String>,
        /// Read the comment from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_comment)] body_file: Option<String>,
        #[arg(long)] author: Option<String>,
    },
    Comments { #[arg(allow_negative_numbers = true)] id: IssueRef },
}

#[derive(Subcommand)]
enum DocCmd {
    Create {
        #[arg(long)] title: String,
        /// The content inline. For long text, use --content-file.
        #[arg(long, value_parser = inline_content, conflicts_with = "content_file")] content: Option<String>,
        /// Read the content from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_content)] content_file: Option<String>,
        #[arg(long)] parent: Option<String>,
    },
    List,
    Show { id: String },
    Update {
        id: String,
        #[arg(long)] title: Option<String>,
        /// The content inline; "" clears it. For long text, use --content-file.
        #[arg(long, value_parser = inline_content, conflicts_with = "content_file")] content: Option<String>,
        /// Read the content from PATH, or from stdin if PATH is `-`. Stored byte for byte.
        #[arg(long, value_name = "PATH", value_parser = read_content)] content_file: Option<String>,
    },
    Delete { id: String },
}

#[derive(Subcommand)]
enum LabelCmd {
    Create { #[arg(value_parser = parse_label_name)] name: String, #[arg(long, default_value = "#888888", value_parser = parse_color)] color: String },
    List,
    /// Delete a label by name (ignoring case) or id, removing it from every issue.
    Delete { #[arg(value_parser = parse_label_name)] name_or_id: String },
}

#[derive(Subcommand)]
enum CycleCmd {
    Create { name: String, #[arg(long, value_parser = parse_date)] start: Option<NaiveDate>, #[arg(long, value_parser = parse_date)] end: Option<NaiveDate> },
    List,
}

/// A real calendar date written exactly as YYYY-MM-DD: 2026-02-30 and
/// 2026-2-3 are both rejected.
fn parse_date(s: &str) -> std::result::Result<NaiveDate, String> {
    let b = s.as_bytes();
    let shaped = b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    shaped
        .then(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .flatten()
        .ok_or_else(|| format!("invalid date \"{s}\"; expected a real calendar date as YYYY-MM-DD"))
}

/// `#rgb` or `#rrggbb`.
fn parse_color(s: &str) -> std::result::Result<String, String> {
    let hex = s.strip_prefix('#').unwrap_or("");
    if s.starts_with('#') && matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(s.to_string())
    } else {
        Err(format!("invalid color \"{s}\"; expected #rgb or #rrggbb"))
    }
}

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
fn parse_label_name(s: &str) -> std::result::Result<String, String> {
    let s = s.trim();
    if s.is_empty() { Err("a label name can't be empty".into()) } else { Ok(s.to_string()) }
}

/// An assignee, trimmed; blank is refused. Unassigning is its own flag.
fn parse_assignee(s: &str) -> std::result::Result<String, String> {
    let s = s.trim();
    if s.is_empty() { Err("an assignee can't be empty; to unassign, use `issue update --no-assignee`".into()) } else { Ok(s.to_string()) }
}

/// `issue list --assignee`: a name, trimmed, or `none` (any case) for
/// unassigned issues. Blank is refused.
fn parse_assignee_filter(s: &str) -> std::result::Result<AssigneeFilter, String> {
    let s = s.trim();
    if s.is_empty() { Err("an assignee can't be empty; for unassigned issues, use --assignee none".into()) }
    else if s.eq_ignore_ascii_case("none") { Ok(AssigneeFilter::Unassigned) }
    else { Ok(AssigneeFilter::Named(s.to_string())) }
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
    let fail = |msg: String| -> ! { Cli::command().error(clap::error::ErrorKind::InvalidSubcommand, msg).exit() };
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
    if let Commands::Project { .. } = cli.cmd {
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
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
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
/// already checked each value on its own; this exits 2 the same way.
fn validate(cli: &Cli) {
    let fail = |msg: String| -> ! { Cli::command().error(clap::error::ErrorKind::ValueValidation, msg).exit() };
    if let Commands::Cycle { cmd: CycleCmd::Create { start: Some(start), end: Some(end), .. } } = &cli.cmd
        && end < start
    {
        fail(format!("cycle end {end} is before its start {start}"));
    }
    // Names match ignoring ASCII case, as label lookups do.
    if let Commands::Issue { cmd: IssueCmd::Update { add_label, remove_label, .. } } = &cli.cmd
        && let Some(both) = add_label.iter().find(|a| remove_label.iter().any(|r| r.eq_ignore_ascii_case(a)))
    {
        fail(format!("label {both:?} is in both --add-label and --remove-label"));
    }
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

/// The workspace a command works in: the global one --workspace names, or
/// else the nearest .lcl/data.sqlite at or above the current directory.
/// Never creates one, and never opens the registry.
fn resolve_workspace(workspace: Option<&str>) -> Result<(PathBuf, Store)> {
    let path = match workspace {
        Some(name) => {
            let slug = registry::slug(name).ok_or_else(|| LclError::NoSlug(name.to_string()))?;
            let path = registry::workspace_path(&home()?, &slug);
            if !path.exists() {
                return Err(LclError::WorkspaceNotFound { name: name.to_string(), path: path.display().to_string() }.into());
            }
            path
        }
        None => find_local_db().ok_or(LclError::NoWorkspace)?,
    };
    let store = Store::open(&path).with_context(|| format!("open workspace {}", path.display()))?;
    Ok((path, store))
}

fn resolve_store(workspace: &Option<String>) -> Result<Store> {
    resolve_workspace(workspace.as_deref()).map(|(_, store)| store)
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

/// Exit 2 for a usage error, as clap does for a bad argument; 1 for anything
/// else. Errors print as `fn main() -> Result` would print them.
fn main() -> ExitCode {
    let mut cli = parse();
    refuse_old_forms(&cli);
    honour_lcl_project(&mut cli);
    validate(&cli);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            let usage = e.downcast_ref::<LclError>().is_some_and(LclError::is_usage);
            ExitCode::from(if usage { 2 } else { 1 })
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Commands::Init { name } => {
            // Before anything is created: init registers what it makes.
            let home = home()?;
            let pname = name.unwrap_or_else(|| {
                std::env::current_dir().ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string())).unwrap_or_else(|| "project".into())
            });
            let key = pname.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_uppercase();
            let key = if key.is_empty() { "PRJ".to_string() } else { key.chars().take(4).collect() };
            let dir = std::env::current_dir()?.join(".lcl");
            std::fs::create_dir_all(&dir)?;
            let dbpath = dir.join("data.sqlite");
            let mut store = Store::open(&dbpath)?;
            let created_at = now().to_string();
            store.set_meta(&[("name", pname.as_str()), ("key", key.as_str()), ("description", ""), ("created_at", created_at.as_str())])?;
            Registry::open(&home)?.register(&pname, &dbpath)?;
            if cli.json { print_json(&serde_json::json!({"path": dbpath, "name": pname, "key": key})); }
            else { println!("Initialized workspace '{}' at {}", pname, dbpath.display()); }
        }
        Commands::Project { .. } => unreachable!("refused by refuse_old_forms"),
        Commands::Workspace { cmd } => match cmd {
            WorkspaceCmd::Create { name, key } => {
                let made = Registry::open(&home()?)?.create_workspace(&name, &key)?;
                if cli.json { print_json(&made); }
                else { println!("Created workspace {:?} (key {}) at {}", made.name, made.key, made.path); }
            }
            WorkspaceCmd::List => {
                let all = Registry::open(&home()?)?.list()?;
                if cli.json { print_json(&all); }
                else {
                    for w in &all { println!("{}\t{}\t{}{}", w.name, w.slug, w.path, if w.missing { "\t(missing)" } else { "" }); }
                }
            }
            WorkspaceCmd::Show => {
                let (path, store) = resolve_workspace(cli.workspace.as_deref())?;
                let name = store.get_meta("name")?.unwrap_or_default();
                let key = store.get_meta("key")?.unwrap_or_default();
                // Stored as text; null if absent or not a number.
                let created_at = store.get_meta("created_at")?.and_then(|t| t.parse::<i64>().ok());
                let schema_version = store.schema_version()?;
                let v = serde_json::json!({"name": name, "key": key, "path": path, "schema_version": schema_version, "created_at": created_at});
                if cli.json { print_json(&v); } else { println!("{} [{}] {}", name, key, path.display()); }
            }
        },
        Commands::Issue { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                IssueCmd::Create { title, description, description_file, status, priority, assignee, parent, label, estimate, due } => {
                    let description = description.or(description_file).unwrap_or_default();
                    let parent_id = parent.map(|p| store.resolve(&p)).transpose()?.map(|p| p.id);
                    let issue = store.create_issue(Issue {
                        id: String::new(), number: 0, identifier: String::new(), title, description, status, priority,
                        assignee, parent_id, parent_identifier: None, estimate, due_date: due.map(|d| d.to_string()), labels: label,
                        created_at: 0, updated_at: 0, completed_at: None, started_at: None, canceled_at: None,
                    })?;
                    if cli.json { print_json(&issue); } else { println!("Created {}", issue.identifier); }
                }
                IssueCmd::List { status, assignee, label, parent, blocked, unblocked, sort, limit, offset, all } => {
                    let page = if all { Page::ALL } else { Page { limit: Some(limit), offset } };
                    let blocked = if blocked { Some(true) } else if unblocked { Some(false) } else { None };
                    let list = store.list_issues(&IssueFilter { statuses: status, assignee, labels: label, parent, blocked }, sort, page)?;
                    if cli.json { print_json(&list); }
                    else {
                        for i in &list.issues { println!("{}\t[{}]\t{}\t{}", i.identifier, i.status, i.priority.label(), i.title); }
                        if list.truncated {
                            let first = list.offset as usize + 1;
                            println!("showing {first}–{} of {}; use --all, or --limit/--offset", first + list.issues.len() - 1, list.total);
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
                        print_json(&serde_json::json!({"issue": issue, "comments": comments, "children": children, "relations": relations}));
                    }
                    else {
                        println!("{} {}", issue.identifier, issue.title);
                        println!("Status: {}  Priority: {}  Assignee: {}", issue.status, issue.priority.label(), issue.assignee.as_deref().unwrap_or("Unassigned"));
                        if !issue.labels.is_empty() { println!("Labels: {}", issue.labels.join(", ")); }
                        if let Some(p) = &issue.parent_identifier { println!("Parent: {p}"); }
                        let line = |name: &str, others: &[RelatedIssue]| {
                            if !others.is_empty() {
                                let others: Vec<String> = others.iter().map(|o| format!("{} [{}]", o.identifier, o.status)).collect();
                                println!("{name}: {}", others.join(", "));
                            }
                        };
                        line("Blocks", &relations.blocks);
                        line("Blocked by", &relations.blocked_by);
                        line("Related", &relations.related);
                        line("Duplicate of", relations.duplicate_of.as_slice());
                        line("Duplicates", &relations.duplicates);
                        if !issue.description.is_empty() { println!("\n{}", issue.description); }
                        if !children.is_empty() {
                            println!("\nSub-issues:");
                            for c in &children { println!("  {} {}", c.identifier, c.title); }
                        }
                        if !comments.is_empty() {
                            println!("\nComments:");
                            for c in comments { println!("  - {}: {}", c.author.as_deref().unwrap_or("anon"), c.body); }
                        }
                    }
                }
                IssueCmd::Update {
                    id, title, description, description_file, status, priority, assignee, no_assignee, estimate, no_estimate, due, no_due,
                    label, add_label, remove_label, clear_labels, parent, no_parent,
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
                    let issue = store.update_issue(&id, |i| {
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
                    })?;
                    if cli.json { print_json(&issue); } else { println!("Updated {}", issue.identifier); }
                }
                IssueCmd::Delete { id } => {
                    let issue = store.delete_issue(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": issue.identifier, "id": issue.id})); } else { println!("Deleted {}", issue.identifier); }
                }
                IssueCmd::Relate { a, kind, b } => {
                    let (relation, created) = store.relate(&a, kind, &b)?;
                    if cli.json { print_json(&serde_json::json!({"issue": relation.issue, "kind": relation.kind, "related": relation.related, "created": created})); }
                    else if !created { println!("Already related: {relation}"); }
                    else if kind == RelationKind::DuplicateOf { println!("{relation}; {} is Duplicate", relation.issue); }
                    else { println!("{relation}"); }
                }
                IssueCmd::Unrelate { a, kind, b } => {
                    let relation = store.unrelate(&a, kind, &b)?;
                    if cli.json { print_json(&serde_json::json!({"issue": relation.issue, "kind": relation.kind, "related": relation.related, "removed": true})); }
                    else { println!("Removed: {relation}"); }
                }
                IssueCmd::Comment { id, body, body_file, author } => {
                    let Some(body) = body.or(body_file) else { unreachable!("clap requires BODY or --body-file") };
                    let c = store.add_comment(&id, body, author)?;
                    if cli.json { print_json(&c); } else { println!("Comment added"); }
                }
                IssueCmd::Comments { id } => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(&issue.id)?;
                    if cli.json { print_json(&comments); }
                    else { for c in comments { println!("[{}] {}: {}", c.created_at, c.author.as_deref().unwrap_or("anon"), c.body); } }
                }
            }
        },
        Commands::Doc { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                DocCmd::Create { title, content, content_file, parent } => {
                    let content = content.or(content_file).unwrap_or_default();
                    let d = store.create_doc(Doc { id: String::new(), title, content, parent_id: parent, created_at: 0, updated_at: 0 })?;
                    if cli.json { print_json(&d); } else { println!("Created doc {}", d.id); }
                }
                DocCmd::List => {
                    let docs = store.list_docs()?;
                    if cli.json { print_json(&docs); }
                    else { for d in docs { println!("{}\t{}", d.id, d.title); } }
                }
                DocCmd::Show { id } => {
                    let d = store.get_doc(&id)?.ok_or_else(|| anyhow::anyhow!("doc not found"))?;
                    if cli.json { print_json(&d); } else { println!("# {}\n\n{}", d.title, d.content); }
                }
                DocCmd::Update { id, title, content, content_file } => {
                    let d = store.update_doc(&id, title, content.or(content_file))?;
                    if cli.json { print_json(&d); } else { println!("Updated {}", id); }
                }
                DocCmd::Delete { id } => {
                    store.delete_doc(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": id})); } else { println!("Deleted {}", id); }
                }
            }
        },
        Commands::Label { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                LabelCmd::Create { name, color } => {
                    let l = Label { id: uuid::Uuid::new_v4().to_string(), name, color };
                    store.create_label(&l)?;
                    if cli.json { print_json(&l); } else { println!("Created label {}", l.name); }
                }
                LabelCmd::List => {
                    let ls = store.list_labels()?;
                    if cli.json { print_json(&ls); } else { for l in ls { println!("{} {} ({})", l.id, l.name, l.color); } }
                }
                LabelCmd::Delete { name_or_id } => {
                    let (label, issues) = store.delete_label(&name_or_id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": label.name, "id": label.id, "removed_from_issues": issues})); }
                    else { println!("Deleted label {} (removed from {} issue{})", label.name, issues, if issues == 1 { "" } else { "s" }); }
                }
            }
        },
        Commands::Cycle { cmd } => {
            let mut store = resolve_store(&cli.workspace)?;
            match cmd {
                CycleCmd::Create { name, start, end } => {
                    let c = store.create_cycle(Cycle { id: String::new(), name, starts_at: start.map(|d| d.to_string()), ends_at: end.map(|d| d.to_string()), created_at: 0 })?;
                    if cli.json { print_json(&c); } else { println!("Created cycle {}", c.id); }
                }
                CycleCmd::List => {
                    let cs = store.list_cycles()?;
                    if cli.json { print_json(&cs); }
                    else {
                        for c in cs {
                            let (start, end) = (c.starts_at.as_deref().unwrap_or("?"), c.ends_at.as_deref().unwrap_or("?"));
                            println!("{}\t{}\t{start}..{end}", c.id, c.name);
                        }
                    }
                }
            }
        },
        Commands::Search { words, limit } => {
            let query = search_query(&words);
            let store = resolve_store(&cli.workspace)?;
            let found = store.search(&query, limit as usize)?;
            if cli.json { print_json(&found); }
            else {
                for h in &found.issues {
                    let fields: Vec<&str> = h.matched_in.iter().map(|f| f.as_str()).collect();
                    println!("{}\t[{}]\t{}\t{}", h.issue.identifier, h.issue.status, h.issue.title, fields.join(", "));
                }
                for d in &found.docs { println!("doc\t{}\t{}", d.id, d.title); }
                if found.issue_total > found.issues.len() { println!("showing {} of {} issues", found.issues.len(), found.issue_total); }
                if found.doc_total > found.docs.len() { println!("showing {} of {} docs", found.docs.len(), found.doc_total); }
                if found.issue_total == 0 && found.doc_total == 0 { println!("no matches"); }
            }
        },
        Commands::Tui => {
            lcl_tui::run(resolve_store(&cli.workspace)?)?;
        }
    }
    Ok(())
}

fn now() -> i64 { lcl_core::models::now() }

// helper to avoid import name clash
