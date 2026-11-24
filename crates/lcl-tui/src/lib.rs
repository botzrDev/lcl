//! A terminal view of a workspace's issues: a list, narrowed by status,
//! project and search, and the detail of one issue. It changes an issue's
//! status and priority; the command line does every other write.
//!
//! The state and its key handling (`App`) are pure, and drawing takes any
//! ratatui backend, so both are tested without a terminal. A key that
//! writes gives the loop an `Edit`, which `write` makes through lcl-core,
//! as `lcl issue update` does. `run` puts the terminal in raw mode on the
//! alternate screen, and `guarded` puts it back on every way out.

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use lcl_core::db::{IssueFilter, IssueSort, IssueUpdate, Page, Placement, Store};
use lcl_core::models::{Comment, Issue, IssueId, IssueRef, Priority, Status};
use lcl_core::search::SearchQuery;
use ratatui::buffer::CellWidth;
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{IsTerminal, stdout};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;

const LIST_HELP: &str = "j/k: move  Enter: open  s: status  !: priority  /: search  p: project  1-7: filter  0: all  r: reload  q: quit";
const DETAIL_HELP: &str = "j/k: scroll  PgUp/PgDn: page  Esc: back  r: reload  q: quit";

/// What marks the selected row. Every row is indented by its width.
const SYMBOL: &str = ">> ";
/// The spaces between two columns of a row.
const GAP: usize = 2;
/// The widest status ("InProgress") and priority ("Urgent", "Medium").
const STATUS_WIDTH: usize = 10;
const PRIORITY_WIDTH: usize = 6;
/// The most cells the project and the assignee columns each take.
const SIDE_MAX: usize = 12;
/// The assignee column, then the project column, is left out while the
/// title would otherwise get fewer cells than this.
const TITLE_MIN: usize = 16;

/// `text` with each control character (a newline, a tab, an escape) turned
/// into a space, so it stays on one row and its words stay apart.
fn clean(text: &str) -> Cow<'_, str> {
    if text.contains(char::is_control) {
        Cow::Owned(
            text.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// How many terminal cells `text` takes once cleaned, as ratatui counts
/// them: a CJK character or an emoji takes two, a combining mark none.
fn cells(text: &str) -> usize {
    let text = clean(text);
    Span::raw(text.as_ref())
        .styled_graphemes(Style::default())
        .map(|g| usize::from(g.symbol.cell_width()))
        .sum()
}

/// `text`, cleaned, in exactly `width` cells: padded with spaces if it is
/// shorter, and cut to end with "…" if it is longer.
fn fit(text: &str, width: usize) -> String {
    let text = clean(text);
    let span = Span::raw(text.as_ref());
    let graphemes = || {
        span.styled_graphemes(Style::default())
            .map(|g| (g.symbol, usize::from(g.symbol.cell_width())))
    };
    let cut = graphemes().map(|(_, w)| w).sum::<usize>() > width;
    let room = if cut { width.saturating_sub(1) } else { width };
    let (mut out, mut used) = (String::new(), 0);
    for (symbol, w) in graphemes() {
        if used + w > room {
            break;
        }
        out.push_str(symbol);
        used += w;
    }
    if cut && width > 0 {
        out.push('…');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

/// `text` in lines of at most `width` cells, counted as `cells` counts
/// them. Each of its lines, cleaned (a tab is a space), is broken at the
/// last space that fits, or inside a word wider than `width`. A blank line
/// stays one empty line; spaces at the end of a line, and at the start of
/// one that continues a broken line, go. Nothing at width 0.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    for line in text.split('\n') {
        let line = clean(line.strip_suffix('\r').unwrap_or(line));
        let span = Span::raw(line.as_ref());
        let (mut cur, mut used) = (String::new(), 0);
        // The byte offset in `cur` of its last space.
        let mut space: Option<usize> = None;
        // Whether `cur` continues a line broken before it.
        let mut continued = false;
        for g in span.styled_graphemes(Style::default()) {
            let (symbol, w) = (g.symbol, usize::from(g.symbol.cell_width()));
            if symbol == " " && (used + w > width || (continued && cur.is_empty())) {
                // The line is full at the end of a word, or a continued
                // line would start with a space.
                if !cur.is_empty() {
                    out.push(cur.trim_end().to_string());
                    (cur, used, space) = (String::new(), 0, None);
                }
                continued = true;
                continue;
            }
            while used + w > width && !cur.is_empty() {
                continued = true;
                match space.take() {
                    // Break at the space, unless only indentation is before it.
                    Some(at) if !cur[..at].trim().is_empty() => {
                        let rest = cur.split_off(at + 1);
                        out.push(cur.trim_end().to_string());
                        cur = rest;
                        used = cells(&cur);
                    }
                    _ => {
                        out.push(std::mem::take(&mut cur));
                        used = 0;
                    }
                }
            }
            if w > width {
                // A wide character in a column one cell wide.
                continue;
            }
            if symbol == " " {
                space = Some(cur.len());
            }
            cur.push_str(symbol);
            used += w;
        }
        out.push(cur.trim_end().to_string());
    }
    out
}

/// Epoch seconds as local time, YYYY-MM-DD HH:MM, as `lcl issue comments`
/// prints them; the number itself if chrono can't represent it.
fn local_time(at: i64) -> String {
    match chrono::DateTime::from_timestamp(at, 0) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => at.to_string(),
    }
}

/// The widths of the columns that depend on what was read: the widest
/// identifier, and the widest project and assignee up to `SIDE_MAX` each
/// (0 when no issue has one, and then the column is not drawn). They are
/// taken over every issue, not the filtered ones, so a filter doesn't move
/// the columns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Widths {
    id: usize,
    project: usize,
    assignee: usize,
}

impl Widths {
    fn of(issues: &[Issue]) -> Widths {
        let widest =
            |text: fn(&Issue) -> &str| issues.iter().map(|i| cells(text(i))).max().unwrap_or(0);
        Widths {
            id: widest(|i| &i.identifier),
            project: widest(|i| i.project.as_deref().unwrap_or("")).min(SIDE_MAX),
            assignee: widest(|i| i.assignee.as_deref().unwrap_or("")).min(SIDE_MAX),
        }
    }

    /// These widths in a row of `width` cells, with the title's as the
    /// fourth: it gets what the other columns leave. While that is under
    /// `TITLE_MIN`, the assignee column goes first, then the project column.
    fn in_row(self, width: usize) -> (Widths, usize) {
        let fixed = self.id + GAP + STATUS_WIDTH + GAP + PRIORITY_WIDTH + GAP;
        let side = |w: usize| if w == 0 { 0 } else { GAP + w };
        let mut shown = self;
        let title = |w: Widths| width.saturating_sub(fixed + side(w.project) + side(w.assignee));
        if title(shown) < TITLE_MIN {
            shown.assignee = 0;
        }
        if title(shown) < TITLE_MIN {
            shown.project = 0;
        }
        (shown, title(shown))
    }
}

/// One read of a workspace: its name and key (either may be empty), every
/// issue in list order, the comments of the issue open in the detail, and
/// what the search found.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub workspace: String,
    pub key: String,
    pub issues: Vec<Issue>,
    /// The comments of the issue that was open (`Request::open`) when this
    /// was read, oldest first; none while the list is shown.
    pub comments: Vec<Comment>,
    /// The search asked for (`Request::search`), if one was.
    pub search: Option<Found>,
}

/// What a search found: its query as typed, and the issues, best first, as
/// `lcl search` ranks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub query: String,
    pub issues: Vec<IssueId>,
}

/// What a reload reads besides the issues (`App::request`).
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// The issue open in the detail, whose comments are read.
    pub open: Option<IssueId>,
    /// The search the list is narrowed by.
    pub search: Option<SearchQuery>,
}

/// A write the TUI asks for: one field of one issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Status(IssueId, Status),
    Priority(IssueId, Priority),
}

/// What the terminal sends: a key, or text pasted in one piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Key(KeyEvent),
    Paste(String),
}

/// What the loop does after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Make this write, then reload; or, if it fails, say why (`App::failed`).
    Write(Edit),
    /// Read the issues again, with what `App::request` asks for (see
    /// `App::reloaded`).
    Reload,
    Quit,
}

/// What the screen shows, and where keys go: the list, the detail of one
/// issue, the search prompt under the list, or a choice of status or
/// priority over it. Each one's state exists only while it is shown.
enum Mode {
    List,
    Detail(Detail),
    Prompt(Prompt),
    Choose(Choice),
}

/// The field a choice sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Status,
    Priority,
}

/// The priorities in their number keys' order, 0 to 4.
const PRIORITIES: [Priority; 5] = [
    Priority::None,
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
];

impl Field {
    /// How many values there are: 7 statuses, 5 priorities.
    fn len(self) -> usize {
        match self {
            Field::Status => Status::all().len(),
            Field::Priority => PRIORITIES.len(),
        }
    }

    /// The value at `at`, named.
    fn name(self, at: usize) -> &'static str {
        match self {
            Field::Status => Status::all()[at].as_str(),
            Field::Priority => PRIORITIES[at].label(),
        }
    }

    /// The number key that picks the value at `at`: statuses 1-7, as the
    /// status filter's, and priorities 0-4, as `issue update` takes them.
    fn digit(self, at: usize) -> usize {
        match self {
            Field::Status => at + 1,
            Field::Priority => at,
        }
    }

    /// Where `issue`'s value is.
    fn of(self, issue: &Issue) -> usize {
        match self {
            Field::Status => Status::all().iter().position(|s| *s == issue.status),
            Field::Priority => PRIORITIES.iter().position(|p| *p == issue.priority),
        }
        .unwrap_or(0)
    }

    /// The write that sets `issue` to the value at `at`.
    fn edit(self, issue: IssueId, at: usize) -> Edit {
        match self {
            Field::Status => Edit::Status(issue, Status::all()[at]),
            Field::Priority => Edit::Priority(issue, PRIORITIES[at]),
        }
    }
}

/// Setting one field of one issue: the values, with one highlighted.
struct Choice {
    issue: IssueId,
    field: Field,
    at: usize,
}

/// A line of text being typed, and the cursor in it: a byte offset on a
/// grapheme boundary, so a wide character, or a letter with its accent, is
/// passed over and deleted whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Prompt {
    text: String,
    cursor: usize,
    /// Why Enter didn't search; the header shows it until the next key.
    error: Option<String>,
}

impl Prompt {
    /// `text`, with the cursor at its end.
    fn new(text: &str) -> Prompt {
        Prompt {
            text: text.into(),
            cursor: text.len(),
            error: None,
        }
    }

    /// Puts `text` at the cursor, cleaned (a pasted newline is a space), and
    /// the cursor after it, or after the grapheme it joined.
    fn insert(&mut self, text: &str) {
        let text = clean(text);
        self.text.insert_str(self.cursor, &text);
        let at = self.cursor + text.len();
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= at)
            .unwrap_or(self.text.len());
    }

    /// Where the grapheme before the cursor starts.
    fn before(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    /// Where the grapheme after the cursor ends.
    fn after(&self) -> usize {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }

    /// Backspace, Delete, the arrows, Home and End edit and move; any other
    /// character without Ctrl or Alt is typed.
    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Backspace => {
                let at = self.before();
                self.text.replace_range(at..self.cursor, "");
                self.cursor = at;
            }
            KeyCode::Delete => {
                let end = self.after();
                self.text.replace_range(self.cursor..end, "");
            }
            KeyCode::Left => self.cursor = self.before(),
            KeyCode::Right => self.cursor = self.after(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(c.encode_utf8(&mut [0; 4]));
            }
            _ => {}
        }
    }
}

/// A project the list is narrowed to: its id, and its name as last read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectFilter {
    id: String,
    name: String,
}

/// The detail of one issue, scrolled to a line.
struct Detail {
    id: IssueId,
    /// The first line shown.
    scroll: usize,
    /// From the last draw: the furthest `scroll` goes (the last line at the
    /// bottom), and how many lines fit, a page. Keys scroll within them.
    bottom: Cell<usize>,
    page: Cell<usize>,
}

impl Detail {
    fn new(id: IssueId) -> Detail {
        Detail {
            id,
            scroll: 0,
            bottom: Cell::new(0),
            page: Cell::new(0),
        }
    }

    /// j/k (or the arrows) scroll a line, PgDn/PgUp (or space and b) a page,
    /// g/G (or Home and End) to the top and the bottom.
    fn scroll(&mut self, code: KeyCode) {
        let (bottom, page) = (self.bottom.get(), self.page.get().max(1));
        // After the terminal grew, `scroll` can be past the bottom.
        let at = self.scroll.min(bottom);
        self.scroll = match code {
            KeyCode::Char('j') | KeyCode::Down => (at + 1).min(bottom),
            KeyCode::Char('k') | KeyCode::Up => at.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => (at + page).min(bottom),
            KeyCode::PageUp | KeyCode::Char('b') => at.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => bottom,
            _ => self.scroll,
        };
    }
}

/// The workspace as last read, what narrows the list (a status, a project,
/// a search), the selected issue, and what the screen shows (`Mode`).
///
/// The selection is the issue's UUID, not its row, so it stays on the same
/// issue when a reload moves the rows. It is `None` exactly when the
/// filtered list is empty; otherwise it names an issue in that list. The
/// issue open in the detail is one the workspace has, filtered out or not.
pub struct App {
    snapshot: Snapshot,
    /// Of `snapshot.issues`; set wherever the snapshot is.
    widths: Widths,
    filter: Option<Status>,
    project: Option<ProjectFilter>,
    /// The search a reload asks for. What it found is `snapshot.search`.
    search: Option<SearchQuery>,
    selected: Option<IssueId>,
    mode: Mode,
    /// Why the last write failed; the keys line shows it until the next key.
    failed: Option<String>,
    /// How a comment's time is shown; local time, but for the tests.
    time: fn(i64) -> String,
}

impl App {
    pub fn new(snapshot: Snapshot) -> App {
        let mut app = App {
            widths: Widths::of(&snapshot.issues),
            snapshot,
            filter: None,
            project: None,
            search: None,
            selected: None,
            mode: Mode::List,
            failed: None,
            time: local_time,
        };
        app.select_row(0);
        app
    }

    /// The issue open in the detail; None while the list is shown. A
    /// reload reads its comments.
    pub fn open(&self) -> Option<&IssueId> {
        match &self.mode {
            Mode::Detail(d) => Some(&d.id),
            Mode::List | Mode::Prompt(_) | Mode::Choose(_) => None,
        }
    }

    /// What the next reload reads: the open issue's comments, and the
    /// search.
    pub fn request(&self) -> Request {
        Request {
            open: self.open().cloned(),
            search: self.search.clone(),
        }
    }

    /// The selected issue's row in the filtered list; 0 when it is empty.
    pub fn row(&self) -> usize {
        self.selected
            .as_ref()
            .and_then(|id| self.visible().iter().position(|i| &i.id == id))
            .unwrap_or(0)
    }

    /// Selects the issue on `row` of the filtered list, or on its last row
    /// if `row` is past it; nothing when the list is empty.
    fn select_row(&mut self, row: usize) {
        let visible = self.visible();
        let id = visible
            .get(row.min(visible.len().saturating_sub(1)))
            .map(|i| i.id.clone());
        self.selected = id;
    }

    /// The issues the status and project filters let through: in list
    /// order, or under a search, the ones it found, best first.
    pub fn visible(&self) -> Vec<&Issue> {
        let keep = |i: &&Issue| {
            self.filter.is_none_or(|s| i.status == s)
                && self
                    .project
                    .as_ref()
                    .is_none_or(|p| i.project_id.as_deref() == Some(p.id.as_str()))
        };
        match &self.snapshot.search {
            None => self.snapshot.issues.iter().filter(keep).collect(),
            Some(found) => {
                let by_id: HashMap<&IssueId, &Issue> =
                    self.snapshot.issues.iter().map(|i| (&i.id, i)).collect();
                found
                    .issues
                    .iter()
                    .filter_map(|id| by_id.get(id).copied())
                    .filter(keep)
                    .collect()
            }
        }
    }

    /// Handles one key. Only presses count, and Ctrl-C quits everywhere.
    /// Any key takes away the message of a failed write.
    ///
    /// At the prompt, keys edit its line (`Prompt::key`); Enter searches
    /// and Esc goes back to the list as it was. Elsewhere q quits. In a
    /// choice, j/k (or the arrows) move, and Enter, or a value's number,
    /// sets it; Esc goes back with nothing written. In the list and the
    /// detail, r asks for a reload. In the list, j/k (or the arrows) move
    /// within it; 1-7 filter by status and 0 clears that filter, p goes to
    /// the next project, and `/` opens the prompt with the search in it; s
    /// and ! open a choice of the selected issue's status and priority; and
    /// Enter opens its detail, asking for a reload to read its comments. A
    /// new filter selects the first row. In the detail, keys scroll
    /// (`Detail::scroll`), and Esc goes back to the list, as it was.
    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::Stay;
        }
        self.failed = None;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        if let Mode::Choose(c) = &mut self.mode {
            let last = c.field.len() - 1;
            let pick = match key.code {
                KeyCode::Char('q') => return Action::Quit,
                KeyCode::Esc => {
                    self.mode = Mode::List;
                    return Action::Stay;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    c.at = (c.at + 1).min(last);
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    c.at = c.at.saturating_sub(1);
                    None
                }
                KeyCode::Enter => Some(c.at),
                KeyCode::Char(d) => d
                    .to_digit(10)
                    .and_then(|d| (0..=last).find(|&at| c.field.digit(at) == d as usize)),
                _ => None,
            };
            return match pick {
                Some(at) => self.set(at),
                None => Action::Stay,
            };
        }
        if let Mode::Prompt(p) = &mut self.mode {
            p.error = None;
            return match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::List;
                    Action::Stay
                }
                KeyCode::Enter => self.submit(),
                _ => {
                    p.key(key);
                    Action::Stay
                }
            };
        }
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('r') => return Action::Reload,
            _ => {}
        }
        match &mut self.mode {
            Mode::List | Mode::Prompt(_) | Mode::Choose(_) => self.list_key(key.code),
            Mode::Detail(_) if key.code == KeyCode::Esc => {
                self.mode = Mode::List;
                Action::Stay
            }
            Mode::Detail(d) => {
                d.scroll(key.code);
                Action::Stay
            }
        }
    }

    fn list_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Enter => match self.selected.clone() {
                Some(id) => {
                    self.mode = Mode::Detail(Detail::new(id));
                    Action::Reload
                }
                None => Action::Stay,
            },
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_row(self.row() + 1);
                Action::Stay
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_row(self.row().saturating_sub(1));
                Action::Stay
            }
            KeyCode::Char(c @ '0'..='7') => {
                self.filter = status_key(c);
                self.select_row(0);
                Action::Stay
            }
            KeyCode::Char('p') => {
                self.next_project();
                Action::Stay
            }
            KeyCode::Char('/') => {
                let text = self.search.as_ref().map_or("", |q| q.raw());
                self.mode = Mode::Prompt(Prompt::new(text));
                Action::Stay
            }
            KeyCode::Char('s') => self.choose(Field::Status),
            KeyCode::Char('!') => self.choose(Field::Priority),
            _ => Action::Stay,
        }
    }

    /// Opens a choice of `field` for the selected issue, at its value.
    fn choose(&mut self, field: Field) -> Action {
        if let Some(issue) = self.selected_issue() {
            self.mode = Mode::Choose(Choice {
                issue: issue.id.clone(),
                field,
                at: field.of(issue),
            });
        }
        Action::Stay
    }

    fn selected_issue(&self) -> Option<&Issue> {
        let id = self.selected.as_ref()?;
        self.snapshot.issues.iter().find(|i| &i.id == id)
    }

    /// Ends a choice on the value at `at`: a write, unless the issue has
    /// that value already.
    fn set(&mut self, at: usize) -> Action {
        let Mode::Choose(c) = std::mem::replace(&mut self.mode, Mode::List) else {
            return Action::Stay;
        };
        let now = self
            .snapshot
            .issues
            .iter()
            .find(|i| i.id == c.issue)
            .map(|i| c.field.of(i));
        if now == Some(at) {
            return Action::Stay;
        }
        Action::Write(c.field.edit(c.issue, at))
    }

    /// A write failed: the keys line says why until the next key, and
    /// nothing else changes.
    pub fn failed(&mut self, why: &str) {
        self.failed = Some(clean(why).into_owned());
    }

    /// Pasted text goes into the prompt; elsewhere it does nothing.
    pub fn paste(&mut self, text: &str) -> Action {
        if let Mode::Prompt(p) = &mut self.mode {
            p.error = None;
            p.insert(text);
        }
        Action::Stay
    }

    /// Enter at the prompt. A query `lcl search` takes becomes the search,
    /// and a reload runs it. An empty line takes the search away. A query
    /// with nothing to search for leaves the prompt open, and says so.
    fn submit(&mut self) -> Action {
        let Mode::Prompt(p) = &mut self.mode else {
            return Action::Stay;
        };
        if p.text.trim().is_empty() {
            self.mode = Mode::List;
            self.search = None;
            self.snapshot.search = None;
            // The list only grows; the selection is still in it.
            if self.selected.is_none() {
                self.select_row(0);
            }
            return Action::Stay;
        }
        match SearchQuery::parse(&p.text) {
            Ok(query) => {
                self.mode = Mode::List;
                self.search = Some(query);
                Action::Reload
            }
            Err(e) => {
                p.error = Some(e.to_string());
                Action::Stay
            }
        }
    }

    /// The projects the issues read are in, by name, then none, in turn.
    fn next_project(&mut self) {
        let mut projects: Vec<(String, &str, &str)> = self
            .snapshot
            .issues
            .iter()
            .filter_map(|i| {
                let (id, name) = (i.project_id.as_deref()?, i.project.as_deref()?);
                Some((name.to_lowercase(), name, id))
            })
            .collect();
        projects.sort();
        projects.dedup();
        let at = self
            .project
            .as_ref()
            .and_then(|p| projects.iter().position(|(_, _, id)| *id == p.id));
        let next = match at {
            None => projects.first(),
            Some(n) => projects.get(n + 1),
        };
        self.project = next.map(|(_, name, id)| ProjectFilter {
            id: id.to_string(),
            name: name.to_string(),
        });
        self.select_row(0);
    }

    /// Takes a fresh read of the workspace, keeping the filters and the
    /// selected issue, whatever row it is on now. If that issue is no longer
    /// in the filtered list, the selection goes to the row it was on (else
    /// the last). A search other than the last selects its best hit. The
    /// detail, or a choice, stays on its issue, or goes back to the list if
    /// the workspace no longer has it (deleted, or purged).
    pub fn reloaded(&mut self, snapshot: Snapshot) {
        let row = self.row();
        let query = |s: &Snapshot| s.search.as_ref().map(|f| f.query.clone());
        let new_search = query(&snapshot) != query(&self.snapshot);
        self.widths = Widths::of(&snapshot.issues);
        self.snapshot = snapshot;
        if let Some(p) = &mut self.project
            && let Some(name) = self
                .snapshot
                .issues
                .iter()
                .find(|i| i.project_id.as_deref() == Some(p.id.as_str()))
                .and_then(|i| i.project.clone())
        {
            p.name = name;
        }
        if new_search {
            self.select_row(0);
        } else if !self
            .visible()
            .iter()
            .any(|i| Some(&i.id) == self.selected.as_ref())
        {
            self.select_row(row);
        }
        let shown = match &self.mode {
            Mode::Detail(d) => Some(&d.id),
            Mode::Choose(c) => Some(&c.issue),
            Mode::List | Mode::Prompt(_) => None,
        };
        if let Some(id) = shown
            && !self.snapshot.issues.iter().any(|i| &i.id == id)
        {
            self.mode = Mode::List;
        }
    }

    /// The header line: the workspace's name, with its key in brackets if it
    /// has one; how many issues it has; and when the list is narrowed, how
    /// many of them are listed, and by what status, project and search.
    /// `demo (D) · 2 of 5 issues · status: Done · project: tui · search: login`.
    /// At the prompt, after an Enter that didn't search, it says why.
    fn header(&self) -> String {
        if let Mode::Prompt(Prompt { error: Some(e), .. }) = &self.mode {
            return clean(e).into_owned();
        }
        let mut out = self.snapshot.workspace.clone();
        if !self.snapshot.key.is_empty() {
            out.push_str(&format!(" ({})", self.snapshot.key));
        }
        let total = self.snapshot.issues.len();
        let issues = if total == 1 { "issue" } else { "issues" };
        let search = self.snapshot.search.as_ref();
        if self.filter.is_none() && self.project.is_none() && search.is_none() {
            out.push_str(&format!(" · {total} {issues}"));
        } else {
            out.push_str(&format!(" · {} of {total} {issues}", self.visible().len()));
        }
        if let Some(status) = self.filter {
            out.push_str(&format!(" · status: {status}"));
        }
        if let Some(p) = &self.project {
            out.push_str(&format!(" · project: {}", clean(&p.name)));
        }
        if let Some(found) = search {
            out.push_str(&format!(" · search: {}", found.query));
        }
        out
    }

    /// The filtered list's rows for a list `width` cells wide, one per issue
    /// and each on one line: the identifier, the status, the priority, the
    /// title, and (see `Widths`) the project and the assignee, in columns
    /// `GAP` apart. A row fills the width left of `SYMBOL` exactly, unless
    /// that is too narrow for the columns before the title.
    #[cfg(test)]
    fn rows(&self, width: usize) -> Vec<String> {
        self.rows_in(width, 0, usize::MAX)
    }

    /// `rows`, but only the `count` rows from row `first` on: a frame shows
    /// a screenful, and laying out every row of a long list for each one
    /// makes every key slow.
    fn rows_in(&self, width: usize, first: usize, count: usize) -> Vec<String> {
        // The highlight symbol is drawn in this width, in front of the row.
        let (w, title) = self.widths.in_row(width.saturating_sub(cells(SYMBOL)));
        let gap = " ".repeat(GAP);
        self.visible()
            .iter()
            .skip(first)
            .take(count)
            .map(|i| {
                let mut row = [
                    fit(&i.identifier, w.id),
                    fit(i.status.as_str(), STATUS_WIDTH),
                    fit(i.priority.label(), PRIORITY_WIDTH),
                    fit(&i.title, title),
                ]
                .join(&gap);
                for (text, width) in [(&i.project, w.project), (&i.assignee, w.assignee)] {
                    if width > 0 {
                        row.push_str(&gap);
                        row.push_str(&fit(text.as_deref().unwrap_or(""), width));
                    }
                }
                row
            })
            .collect()
    }

    /// The open issue's detail, in lines of at most `width` cells: its
    /// identifier and title; its status, priority, assignee and labels; its
    /// description; then its comments, oldest first, each `AUTHOR · TIME`
    /// and its body indented two cells, as `lcl issue comments` prints them.
    fn detail_lines(&self, issue: &Issue, width: usize) -> Vec<Line<'static>> {
        let plain = Style::default();
        let bold = plain.add_modifier(Modifier::BOLD);
        let dim = plain.add_modifier(Modifier::DIM);
        let mut out = Vec::new();
        let mut add = |text: &str, style: Style, indent: usize| {
            for line in wrap(text, width.saturating_sub(indent)) {
                out.push(Line::styled(format!("{}{line}", " ".repeat(indent)), style));
            }
        };
        add(&format!("{}  {}", issue.identifier, issue.title), bold, 0);
        add("", plain, 0);
        let labels = issue.labels.join(", ");
        for (name, value) in [
            ("Status", issue.status.as_str()),
            ("Priority", issue.priority.label()),
            ("Assignee", issue.assignee.as_deref().unwrap_or("none")),
            ("Labels", if labels.is_empty() { "none" } else { &labels }),
        ] {
            add(&format!("{name:<8}  {value}"), plain, 0);
        }
        add("", plain, 0);
        if issue.description.trim().is_empty() {
            add("No description.", dim, 0);
        } else {
            add(&issue.description, plain, 0);
        }
        add("", plain, 0);
        let comments: Vec<&Comment> = self
            .snapshot
            .comments
            .iter()
            .filter(|c| c.issue_id == issue.id.as_str())
            .collect();
        if comments.is_empty() {
            add("No comments.", dim, 0);
        }
        for (n, c) in comments.iter().enumerate() {
            if n == 0 {
                add(&format!("Comments ({})", comments.len()), bold, 0);
            }
            add("", plain, 0);
            let author = c.author.as_deref().unwrap_or("anon");
            add(&format!("{author} · {}", (self.time)(c.created_at)), dim, 0);
            add(&c.body, plain, 2);
        }
        out
    }

    /// One line of header, the list or the open issue's detail, one line
    /// of keys.
    pub fn draw(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(f.area());
        f.render_widget(
            Paragraph::new(self.header()).style(Style::default().add_modifier(Modifier::BOLD)),
            chunks[0],
        );
        let help: Cow<str> = match &self.mode {
            Mode::List => {
                self.draw_list(f, chunks[1]);
                LIST_HELP.into()
            }
            Mode::Detail(d) => {
                self.draw_detail(f, chunks[1], d);
                DETAIL_HELP.into()
            }
            Mode::Prompt(p) => {
                self.draw_list(f, chunks[1]);
                draw_prompt(f, chunks[2], p);
                return;
            }
            Mode::Choose(c) => {
                self.draw_list(f, chunks[1]);
                self.draw_choice(f, chunks[1], c);
                let (first, last) = (c.field.digit(0), c.field.digit(c.field.len() - 1));
                format!("j/k: move  Enter or {first}-{last}: set  Esc: cancel").into()
            }
        };
        let line = match &self.failed {
            Some(why) => Paragraph::new(format!("Not written: {why}"))
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            None => Paragraph::new(help).style(Style::default().add_modifier(Modifier::DIM)),
        };
        f.render_widget(line, chunks[2]);
    }

    /// The choice in a box over the list: a title naming the issue and the
    /// field, then each value with its number key, the issue's own marked.
    fn draw_choice(&self, f: &mut Frame, area: Rect, c: &Choice) {
        let Some(issue) = self.snapshot.issues.iter().find(|i| i.id == c.issue) else {
            return;
        };
        let field = match c.field {
            Field::Status => "status",
            Field::Priority => "priority",
        };
        let title = format!(" {} {field} ", clean(&issue.identifier));
        let now = c.field.of(issue);
        let rows: Vec<String> = (0..c.field.len())
            .map(|at| {
                let mark = if at == now { " (now)" } else { "" };
                format!("{}  {}{mark}", c.field.digit(at), c.field.name(at))
            })
            .collect();
        let inner = rows
            .iter()
            .map(|r| cells(r))
            .chain([cells(&title)])
            .max()
            .unwrap_or(0)
            + cells(SYMBOL);
        // Two cells of border, then as much as fits.
        let width = u16::try_from(inner + 2).unwrap_or(u16::MAX).min(area.width);
        let height = u16::try_from(rows.len() + 2)
            .unwrap_or(u16::MAX)
            .min(area.height);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        let list = List::new(rows.into_iter().map(ListItem::new))
            .block(Block::bordered().title(title))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol(SYMBOL);
        let mut state = ListState::default();
        state.select(Some(c.at));
        f.render_widget(Clear, rect);
        f.render_stateful_widget(list, rect, &mut state);
    }

    /// The detail's lines that fit in `area`, from its scroll on; it keeps
    /// how far it can scroll, and a page, for the keys.
    fn draw_detail(&self, f: &mut Frame, area: Rect, d: &Detail) {
        let Some(issue) = self.snapshot.issues.iter().find(|i| i.id == d.id) else {
            return;
        };
        let lines = self.detail_lines(issue, area.width.into());
        let height = usize::from(area.height);
        let bottom = lines.len().saturating_sub(height);
        d.bottom.set(bottom);
        d.page.set(height);
        let shown: Vec<Line> = lines
            .into_iter()
            .skip(d.scroll.min(bottom))
            .take(height)
            .collect();
        f.render_widget(Paragraph::new(shown), area);
    }

    fn draw_list(&self, f: &mut Frame, area: Rect) {
        // The rows on screen: from the top while the selection fits there,
        // and after that the ones that end with it.
        let height = usize::from(area.height);
        let row = self.row();
        let first = (row + 1).saturating_sub(height.max(1));

        // A `Line` is one row whatever its text holds.
        let items: Vec<ListItem> = self
            .rows_in(area.width.into(), first, height)
            .into_iter()
            .map(|row| ListItem::new(Line::from(row)))
            .collect();
        let list = List::new(items)
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol(SYMBOL);
        let mut state = ListState::default();
        state.select(Some(row - first));
        f.render_stateful_widget(list, area, &mut state);
    }
}

/// The prompt's line, `/` and its text, with the cursor on it. While the
/// line is wider than `area`, its start is left out, so the cursor stays on
/// screen with a cell for it.
fn draw_prompt(f: &mut Frame, area: Rect, p: &Prompt) {
    let width = usize::from(area.width);
    if width == 0 {
        return;
    }
    let line = format!("/{}", p.text);
    let cursor = 1 + cells(&p.text[..p.cursor]);
    let over = (cursor + 1).saturating_sub(width);
    let (mut start, mut dropped) = (line.len(), 0);
    for (i, g) in line.grapheme_indices(true) {
        if dropped >= over {
            start = i;
            break;
        }
        dropped += cells(g);
    }
    f.render_widget(Paragraph::new(&line[start..]), area);
    // `cursor - dropped` is under `width`, which is a u16.
    let x = u16::try_from(cursor - dropped).unwrap_or(area.width - 1);
    f.set_cursor_position((area.x + x, area.y));
}

/// The status a number key filters by: 1 Backlog … 7 Duplicate; 0 none.
fn status_key(c: char) -> Option<Status> {
    match c {
        '1' => Some(Status::Backlog),
        '2' => Some(Status::Todo),
        '3' => Some(Status::InProgress),
        '4' => Some(Status::InReview),
        '5' => Some(Status::Done),
        '6' => Some(Status::Canceled),
        '7' => Some(Status::Duplicate),
        _ => None,
    }
}

/// Runs the TUI on an open workspace; the caller resolves which one. The
/// issues are read before the terminal is touched, so a database error ends
/// lcl with that error, as one on a reload does; the terminal is restored
/// first (see `guarded`).
pub fn run(store: Store) -> Result<()> {
    let mut app = App::new(load(&store, &Request::default())?);
    let store = RefCell::new(store);
    // Said here, in words: raw mode on something that isn't a terminal
    // fails with the system's "No such device or address".
    if !(std::io::stdin().is_terminal() && stdout().is_terminal()) {
        return Err(lcl_core::error::LclError::NoTerminal.into());
    }
    guarded(enter_terminal, leave_terminal, || {
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        run_app(
            &mut terminal,
            &mut app,
            |r| load(&store.borrow(), r),
            |e| write(&mut store.borrow_mut(), e),
            next_input,
        )
    })
}

/// The workspace's name and key, and every issue by number; then what
/// `request` asks for: the open issue's comments, oldest first, and the
/// issues the search finds, as `lcl search` finds and ranks them. Read-only.
fn load(store: &Store, request: &Request) -> Result<Snapshot> {
    let issues = store
        .list_issues(&IssueFilter::default(), IssueSort::Number, Page::ALL)?
        .issues;
    let search = match &request.search {
        Some(query) => Some(Found {
            query: query.raw().to_string(),
            issues: store
                .search(query, issues.len())?
                .issues
                .into_iter()
                .map(|hit| hit.issue.id)
                .collect(),
        }),
        None => None,
    };
    Ok(Snapshot {
        workspace: store.get_meta("name")?.unwrap_or_default(),
        key: store.get_meta("key")?.unwrap_or_default(),
        issues,
        comments: match &request.open {
            Some(id) => store.list_comments(id.as_str(), false)?,
            None => Vec::new(),
        },
        search,
    })
}

/// Makes `edit` with `Store::update_issue`, as `lcl issue update` does: one
/// transaction, which the change log records under the store's author.
fn write(store: &mut Store, edit: &Edit) -> Result<()> {
    let (id, update) = match edit {
        Edit::Status(id, status) => (
            id,
            IssueUpdate {
                status: Some(*status),
                ..Default::default()
            },
        ),
        Edit::Priority(id, priority) => (
            id,
            IssueUpdate {
                priority: Some(*priority),
                ..Default::default()
            },
        ),
    };
    let issue = IssueRef::Id(id.as_str().to_string());
    store.update_issue(&issue, &Placement::default(), &update)?;
    Ok(())
}

/// The next key or paste, or None if none comes within a quarter second.
fn next_input() -> Result<Option<Input>> {
    if event::poll(Duration::from_millis(250))? {
        return Ok(match event::read()? {
            Event::Key(key) => Some(Input::Key(key)),
            Event::Paste(text) => Some(Input::Paste(text)),
            _ => None,
        });
    }
    Ok(None)
}

/// Draws, then acts on the next key or paste, until a quit. A reload is
/// given what the app asks for (`App::request`). A write is followed by a
/// reload; if it fails, the app says why (`App::failed`) and goes on. An
/// error drawing, reading a key or reloading ends the loop with that error.
fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut reload: impl FnMut(&Request) -> Result<Snapshot>,
    mut write: impl FnMut(&Edit) -> Result<()>,
    mut next_input: impl FnMut() -> Result<Option<Input>>,
) -> Result<()>
where
    B::Error: Send + Sync + 'static,
{
    loop {
        terminal.draw(|f| app.draw(f))?;
        let action = match next_input()? {
            Some(Input::Key(key)) => app.key(key),
            Some(Input::Paste(text)) => app.paste(&text),
            None => continue,
        };
        match action {
            Action::Quit => return Ok(()),
            Action::Reload => {
                let snapshot = reload(&app.request())?;
                app.reloaded(snapshot);
            }
            Action::Write(edit) => match write(&edit) {
                Ok(()) => {
                    let snapshot = reload(&app.request())?;
                    app.reloaded(snapshot);
                }
                Err(e) => app.failed(&e.to_string()),
            },
            Action::Stay => {}
        }
    }
}

/// Runs `body` after `enter`, and `leave` after it however it ends: by
/// returning, with an error, or in a panic. If `enter` fails, neither runs
/// (`enter` undoes its own part way).
fn guarded<T>(
    enter: impl FnOnce() -> Result<()>,
    leave: impl FnOnce(),
    body: impl FnOnce() -> Result<T>,
) -> Result<T> {
    struct OnDrop<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for OnDrop<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    enter()?;
    let _leave = OnDrop(Some(leave));
    body()
}

/// Raw mode, then the alternate screen; raw mode is undone if the second
/// fails. Then bracketed paste, so a paste comes as one piece, not as keys
/// (a newline in it would be Enter); a terminal without it still works.
fn enter_terminal() -> Result<()> {
    enable_raw_mode()?;
    if let Err(e) = stdout().execute(EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    let _ = stdout().execute(EnableBracketedPaste);
    Ok(())
}

/// Leaves bracketed paste, the alternate screen and raw mode. Best effort:
/// on the way out there is nowhere to report a failure.
fn leave_terminal() {
    let _ = stdout().execute(DisableBracketedPaste);
    let _ = stdout().execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcl_core::db::NewIssue;
    use lcl_core::models::Priority;
    use ratatui::backend::TestBackend;
    use std::cell::RefCell;

    fn issue(number: i64, status: Status) -> Issue {
        Issue {
            id: IssueId::from_stored(format!("id-{number}")),
            number,
            identifier: format!("W-{number}"),
            title: format!("issue {number}"),
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
            created_at: 0,
            updated_at: 0,
            completed_at: None,
            started_at: None,
            canceled_at: None,
            deleted_at: None,
        }
    }

    /// Workspace "work" (key W) with these issues.
    fn work(issues: Vec<Issue>) -> Snapshot {
        Snapshot {
            workspace: "work".into(),
            key: "W".into(),
            issues,
            comments: vec![],
            search: None,
        }
    }

    /// W-1 Done, W-2 Backlog, W-3 Done, W-4 Backlog, W-5 Backlog.
    fn five() -> Snapshot {
        work(
            [
                Status::Done,
                Status::Backlog,
                Status::Done,
                Status::Backlog,
                Status::Backlog,
            ]
            .into_iter()
            .enumerate()
            .map(|(n, s)| issue(n as i64 + 1, s))
            .collect(),
        )
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn selected(app: &App) -> &str {
        &app.visible()[app.row()].identifier
    }

    #[test]
    fn j_stops_at_the_filtered_list_s_last_row() {
        let mut app = App::new(five());
        assert_eq!(app.key(press('5')), Action::Stay);
        for _ in 0..4 {
            assert_eq!(app.key(press('j')), Action::Stay);
        }
        assert_eq!(
            (app.visible().len(), app.row(), selected(&app)),
            (2, 1, "W-3")
        );
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.row(), 1);
        app.key(press('k'));
        assert_eq!(selected(&app), "W-1");
    }

    #[test]
    fn changing_the_filter_selects_the_first_row() {
        let mut app = App::new(five());
        for _ in 0..4 {
            app.key(press('j'));
        }
        assert_eq!(selected(&app), "W-5");
        app.key(press('1'));
        assert_eq!((app.row(), selected(&app)), (0, "W-2"));
        app.key(press('j'));
        app.key(press('0'));
        assert_eq!((app.row(), selected(&app)), (0, "W-1"));
        // A status no issue has: an empty list, and j stays on row 0.
        app.key(press('7'));
        app.key(press('j'));
        assert_eq!(
            (app.visible().len(), app.row(), &app.selected),
            (0, 0, &None)
        );
    }

    #[test]
    fn r_asks_for_a_reload_and_keeps_the_filter() {
        let mut app = App::new(five());
        app.key(press('1'));
        app.key(press('j'));
        app.key(press('j'));
        assert_eq!(app.key(press('r')), Action::Reload);
        assert_eq!(selected(&app), "W-5");
        // W-5 is gone: the selection moves up to the last row left.
        app.reloaded(work(five().issues.into_iter().take(4).collect()));
        assert_eq!((app.visible().len(), selected(&app)), (2, "W-4"));
    }

    /// Issues `numbers`, in that order, all Backlog.
    fn numbered(numbers: &[i64]) -> Snapshot {
        work(numbers.iter().map(|n| issue(*n, Status::Backlog)).collect())
    }

    #[test]
    fn a_reload_keeps_the_selected_issue_not_its_row() {
        let mut app = App::new(numbered(&[5, 4, 3, 2, 1]));
        app.key(press('j'));
        app.key(press('j'));
        assert_eq!((app.row(), selected(&app)), (2, "W-3"));
        // Another process made W-6, which lists first: W-3 is now a row down.
        app.reloaded(numbered(&[6, 5, 4, 3, 2, 1]));
        assert_eq!((app.row(), selected(&app)), (3, "W-3"));
        // W-3 is deleted: the selection stays on the row it had.
        app.reloaded(numbered(&[6, 5, 4, 2, 1]));
        assert_eq!((app.row(), selected(&app)), (3, "W-2"));
        // That row is gone too: the last row.
        app.reloaded(numbered(&[6, 5]));
        assert_eq!((app.row(), selected(&app)), (1, "W-5"));
        app.reloaded(work(vec![]));
        assert_eq!((app.row(), &app.selected), (0, &None));
        // From an empty list, a reload selects the first row.
        app.reloaded(numbered(&[2, 1]));
        assert_eq!(selected(&app), "W-2");
    }

    /// Every sequence of up to four keys and reloads, from three starting
    /// lists, among them a search, a project filter and choices. After each
    /// one: the selection is None exactly when the filtered list is empty,
    /// else an issue in it; a reload with the same search that keeps the
    /// selected issue in the filtered list leaves it selected; and an open
    /// issue, or one being set, is one the workspace has.
    #[test]
    fn the_selection_is_always_an_issue_in_the_filtered_list() {
        // A search that found two issues, and one this read doesn't have.
        let mut found = four();
        found.search = Some(Found {
            query: "x".into(),
            issues: ["id-112", "id-1", "id-999"]
                .map(|id| IssueId::from_stored(id.to_string()))
                .to_vec(),
        });
        let reloads = [work(vec![]), found, numbered(&[6, 5, 4, 3, 2, 1])];
        let keys = [
            press('j'),
            press('k'),
            press('1'),
            press('7'),
            press('0'),
            press('p'),
            press('/'),
            press('s'),
            press('!'),
            code(KeyCode::Enter),
            code(KeyCode::Esc),
        ];
        let ops = keys.len() + reloads.len();
        let apply = |app: &mut App, op: usize| {
            if op < keys.len() {
                app.key(keys[op]);
            } else {
                let before = app.selected.clone();
                let query = |s: &Snapshot| s.search.as_ref().map(|f| f.query.clone());
                let same_search = query(&app.snapshot) == query(&reloads[op - keys.len()]);
                app.reloaded(reloads[op - keys.len()].clone());
                if same_search
                    && before.is_some()
                    && app.visible().iter().any(|i| Some(&i.id) == before.as_ref())
                {
                    assert_eq!(
                        app.selected, before,
                        "a reload moved the selection off an issue still listed"
                    );
                }
            }
            let visible = app.visible();
            match &app.selected {
                None => assert!(
                    visible.is_empty(),
                    "nothing selected in a list of {}",
                    visible.len()
                ),
                Some(id) => assert!(
                    visible.iter().any(|i| &i.id == id),
                    "{id} is selected but not listed"
                ),
            }
            if let Some(id) = app.open() {
                assert!(
                    app.snapshot.issues.iter().any(|i| &i.id == id),
                    "{id} is open but the workspace doesn't have it"
                );
            }
            if let Mode::Choose(c) = &app.mode {
                assert!(
                    app.snapshot.issues.iter().any(|i| i.id == c.issue) && c.at < c.field.len(),
                    "a choice for {} at {}",
                    c.issue,
                    c.at
                );
            }
        };
        let mut sequences = 0;
        for start in [work(vec![]), five(), four()] {
            for len in 0..=4u32 {
                for mut n in 0..ops.pow(len) {
                    let mut app = App::new(start.clone());
                    assert_eq!(app.selected.is_none(), start.issues.is_empty());
                    for _ in 0..len {
                        apply(&mut app, n % ops);
                        n /= ops;
                    }
                    sequences += 1;
                }
            }
        }
        // 1 + 14 + 14^2 + 14^3 + 14^4 sequences from each start.
        assert_eq!(sequences, 3 * 41_371);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = App::new(five());
        assert_eq!(app.key(press('q')), Action::Quit);
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        // c alone, and a key release, do nothing.
        assert_eq!(app.key(press('c')), Action::Stay);
        let mut release = press('q');
        release.kind = KeyEventKind::Release;
        assert_eq!(app.key(release), Action::Stay);
    }

    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// An issue with everything a row shows.
    fn full(
        number: i64,
        status: Status,
        priority: Priority,
        title: &str,
        project: Option<&str>,
        assignee: Option<&str>,
    ) -> Issue {
        Issue {
            priority,
            title: title.into(),
            project_id: project.map(|p| format!("p-{p}")),
            project: project.map(String::from),
            assignee: assignee.map(String::from),
            ..issue(number, status)
        }
    }

    /// Four issues: a long title, one with a tab and a newline in it, and
    /// one with a long project name and no assignee.
    fn four() -> Snapshot {
        work(vec![
            full(
                1,
                Status::Done,
                Priority::Urgent,
                "Fix the login redirect",
                Some("tui"),
                Some("austin"),
            ),
            full(
                2,
                Status::Backlog,
                Priority::None,
                "Write the importer for Linear exports",
                Some("converter"),
                Some("brutus"),
            ),
            full(
                10,
                Status::InProgress,
                Priority::High,
                "Tabs\tand\nnewlines",
                None,
                None,
            ),
            full(
                112,
                Status::Todo,
                Priority::Low,
                "Short",
                Some("a very long project name"),
                None,
            ),
        ])
    }

    fn frame(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        lines(&terminal)
    }

    #[test]
    fn a_frame_is_the_header_the_rows_and_the_keys() {
        let app = App::new(five());
        assert_eq!(
            frame(&app, 60, 8),
            [
                "work (W) · 5 issues                                         ",
                ">> W-1  Done        None    issue 1                         ",
                "   W-2  Backlog     None    issue 2                         ",
                "   W-3  Done        None    issue 3                         ",
                "   W-4  Backlog     None    issue 4                         ",
                "   W-5  Backlog     None    issue 5                         ",
                "                                                            ",
                "j/k: move  Enter: open  s: status  !: priority  /: search  p",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn under_a_filter_the_header_says_how_many_are_listed_and_why() {
        let mut app = App::new(five());
        app.key(press('5'));
        app.key(press('j'));
        assert_eq!(
            frame(&app, 60, 4),
            [
                "work (W) · 2 of 5 issues · status: Done                     ",
                "   W-1  Done        None    issue 1                         ",
                ">> W-3  Done        None    issue 3                         ",
                "j/k: move  Enter: open  s: status  !: priority  /: search  p",
            ]
            .map(String::from)
        );
        // No issue has the status: the count is 0 and the list is empty.
        app.key(press('3'));
        assert_eq!(
            frame(&app, 60, 3)[..2],
            [
                "work (W) · 0 of 5 issues · status: InProgress               ",
                "                                                            "
            ]
            .map(String::from)
        );
        // 0 takes the filter, and its part of the header, away.
        app.key(press('0'));
        assert_eq!(
            frame(&app, 60, 3)[0],
            "work (W) · 5 issues                                         "
        );
    }

    #[test]
    fn the_header_counts_one_issue_and_leaves_out_a_key_that_isn_t_there() {
        let mut app = App::new(Snapshot {
            workspace: "solo".into(),
            key: String::new(),
            issues: vec![issue(1, Status::Todo)],
            comments: vec![],
            search: None,
        });
        assert_eq!(app.header(), "solo · 1 issue");
        app.key(press('2'));
        assert_eq!(app.header(), "solo · 1 of 1 issue · status: Todo");
        // A reload brings the workspace's new name and count.
        app.reloaded(work(vec![]));
        assert_eq!(app.header(), "work (W) · 0 of 0 issues · status: Todo");
    }

    #[test]
    fn rows_are_columns_and_a_long_title_or_project_ends_with_an_ellipsis() {
        let app = App::new(four());
        assert_eq!(
            frame(&app, 70, 7),
            [
                "work (W) · 4 issues                                                   ",
                ">> W-1    Done        Urgent  Fix the login red…  tui           austin",
                "   W-2    Backlog     None    Write the importe…  converter     brutus",
                "   W-10   InProgress  High    Tabs and newlines                       ",
                "   W-112  Todo        Low     Short               a very long…        ",
                "                                                                      ",
                "j/k: move  Enter: open  s: status  !: priority  /: search  p: project ",
            ]
            .map(String::from)
        );
    }

    /// The title keeps 16 cells: below that the assignee column goes, then
    /// the project column. `four` needs 68 cells for all of them.
    #[test]
    fn a_narrow_list_drops_the_assignee_then_the_project() {
        let app = App::new(four());
        let second = |width: u16| frame(&app, width, 4).swap_remove(2);
        assert_eq!(
            second(68),
            "   W-2    Backlog     None    Write the impor…  converter     brutus"
        );
        assert_eq!(
            second(67),
            "   W-2    Backlog     None    Write the importer for…  converter   "
        );
        assert_eq!(
            second(60),
            "   W-2    Backlog     None    Write the impor…  converter   "
        );
        assert_eq!(
            second(59),
            "   W-2    Backlog     None    Write the importer for Linea…"
        );
        // Narrower than the columns before the title: the row is cut off.
        assert_eq!(second(20), "   W-2    Backlog   ");
    }

    /// A column no issue fills is not drawn, and the widths come from every
    /// issue read, so a filter doesn't move the columns but a reload can.
    #[test]
    fn the_columns_fit_what_was_read_not_what_is_listed() {
        let long = "a title long enough to reach the right edge of the list";
        let mut app = App::new(work(vec![
            full(1, Status::Done, Priority::None, long, None, None),
            issue(100, Status::Backlog),
        ]));
        assert_eq!(
            app.widths,
            Widths {
                id: 5,
                project: 0,
                assignee: 0
            }
        );
        app.key(press('5'));
        assert_eq!(
            frame(&app, 50, 3)[1],
            ">> W-1    Done        None    a title long enough…"
        );
        app.reloaded(work(vec![full(
            1,
            Status::Done,
            Priority::None,
            long,
            None,
            Some("al"),
        )]));
        assert_eq!(
            app.widths,
            Widths {
                id: 3,
                project: 0,
                assignee: 2
            }
        );
        assert_eq!(
            frame(&app, 50, 3)[1],
            ">> W-1  Done        None    a title long enou…  al"
        );
    }

    #[test]
    fn fit_cleans_cuts_and_pads_to_the_width() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcde", 5), "abcde");
        assert_eq!(fit("abcdef", 5), "abcd…");
        assert_eq!(fit("abc", 1), "…");
        assert_eq!(fit("abc", 0), "");
        assert_eq!(fit("", 3), "   ");
        // Control characters become spaces, one each.
        assert_eq!(fit("a\tb\nc", 5), "a b c");
        assert_eq!(fit("a\r\nb", 4), "a  b");
        assert_eq!(fit("\x1b[31mred\x07\u{85}", 10), " [31mred  ");
        // A wide character takes two cells, and is not cut in half.
        assert_eq!(
            (cells("日本語"), fit("日本語", 6)),
            (6, "日本語".to_string())
        );
        assert_eq!(fit("日本語", 5), "日本…");
        assert_eq!(fit("日本語", 4), "日… ");
        // A combining mark takes none, and stays with its letter.
        assert_eq!(
            (
                cells("e\u{301}x"),
                fit("e\u{301}x", 2),
                fit("e\u{301}xy", 2)
            ),
            (2, "e\u{301}x".to_string(), "e\u{301}…".to_string())
        );
    }

    #[test]
    fn the_status_and_priority_columns_are_as_wide_as_the_widest_name() {
        assert_eq!(
            Status::all().iter().map(|s| s.as_str().len()).max(),
            Some(STATUS_WIDTH)
        );
        let priorities = [
            Priority::None,
            Priority::Urgent,
            Priority::High,
            Priority::Medium,
            Priority::Low,
        ];
        assert_eq!(
            priorities.iter().map(|p| p.label().len()).max(),
            Some(PRIORITY_WIDTH)
        );
    }

    /// At every width from 0 to 120 and every height from 0 to 8, with
    /// titles, projects and assignees that hold newlines, tabs and wide
    /// characters: drawing works, each issue takes exactly one line, in
    /// order, and a row fills the list's width exactly.
    #[test]
    fn every_issue_takes_exactly_one_row_at_every_size() {
        let mut snapshot = four();
        snapshot.issues.push(full(
            7,
            Status::InReview,
            Priority::Medium,
            "one\ntwo\r\nthree\tfour 日本語のタイトル e\u{301}",
            Some("pro\nject"),
            Some("a\tb"),
        ));
        snapshot.issues.push(full(
            8,
            Status::Canceled,
            Priority::None,
            "",
            Some(""),
            Some("日本語日本語日本語"),
        ));
        let app = App::new(snapshot);
        let ids = ["W-1", "W-2", "W-10", "W-112", "W-7", "W-8"];
        for width in 0..=120u16 {
            let rows = app.rows(width.into());
            assert_eq!(rows.len(), ids.len());
            for row in &rows {
                assert!(!row.contains(char::is_control), "{row:?}");
                if width >= 30 {
                    assert_eq!(cells(row), usize::from(width) - 3, "width {width}: {row:?}");
                }
            }
            for height in 0..=8u16 {
                let lines = frame(&app, width, height);
                // Without the header and the keys.
                for (y, line) in lines
                    .iter()
                    .skip(1)
                    .take(usize::from(height).saturating_sub(2))
                    .enumerate()
                {
                    let id = ids.get(y).map(|id| format!("{id} ")).unwrap_or_default();
                    let shown: String = line.chars().skip(3).collect();
                    if width >= 9 {
                        assert!(
                            shown.starts_with(&id) && (y < ids.len() || shown.trim().is_empty()),
                            "{width}x{height}, line {y}: {line:?}"
                        );
                    }
                }
            }
        }
    }

    /// `load` on a real (in-memory) workspace: its name and key from meta,
    /// and its issues, highest number first.
    #[test]
    fn load_reads_the_name_the_key_and_every_issue() {
        let mut store = Store::open_in_memory().unwrap();
        let empty = load(&store, &Request::default()).unwrap();
        assert_eq!(
            (
                empty.workspace.as_str(),
                empty.key.as_str(),
                empty.issues.len()
            ),
            ("", "", 0)
        );
        store.set_meta(&[("name", "demo"), ("key", "D")]).unwrap();
        for title in ["one", "two"] {
            store
                .create_issue(
                    NewIssue {
                        title: title.into(),
                        status: Status::Todo,
                        ..Default::default()
                    },
                    &Default::default(),
                )
                .unwrap();
        }
        let snapshot = load(&store, &Request::default()).unwrap();
        let rows: Vec<(&str, &str)> = snapshot
            .issues
            .iter()
            .map(|i| (i.identifier.as_str(), i.title.as_str()))
            .collect();
        assert_eq!(
            (snapshot.workspace.as_str(), snapshot.key.as_str(), rows),
            ("demo", "D", vec![("D-2", "two"), ("D-1", "one")])
        );

        // An issue in the trash is not shown; restored, it is again.
        let two = lcl_core::models::IssueRef::Number {
            key: None,
            number: 2,
        };
        store.delete_issue(&two).unwrap();
        let ids = |store: &Store| -> Vec<String> {
            load(store, &Request::default())
                .unwrap()
                .issues
                .iter()
                .map(|i| i.identifier.clone())
                .collect()
        };
        assert_eq!(ids(&store), ["D-1"]);
        store.restore_issue(&two).unwrap();
        assert_eq!(ids(&store), ["D-2", "D-1"]);
    }

    /// For a loop that is given no write to make.
    fn no_write(edit: &Edit) -> Result<()> {
        panic!("wrote {edit:?}")
    }

    /// Keys from a list, then an error if the loop asks for more.
    fn keys(list: &[KeyEvent]) -> impl FnMut() -> Result<Option<Input>> + '_ {
        let mut it = list.iter();
        move || {
            it.next()
                .map(|k| Some(Input::Key(*k)))
                .ok_or_else(|| anyhow::anyhow!("out of keys"))
        }
    }

    #[test]
    fn the_loop_quits_reloads_and_ends_with_a_reload_s_error() {
        let mut terminal = Terminal::new(TestBackend::new(48, 9)).unwrap();
        let mut app = App::new(five());
        let reloads = RefCell::new(0);
        let reload = |_: &Request| {
            *reloads.borrow_mut() += 1;
            Ok(five())
        };
        run_app(
            &mut terminal,
            &mut app,
            reload,
            no_write,
            keys(&[press('r'), press('j'), press('q')]),
        )
        .unwrap();
        assert_eq!((*reloads.borrow(), app.row()), (1, 1));

        let failing = |_: &Request| Err(anyhow::anyhow!("database is locked"));
        let e = run_app(
            &mut terminal,
            &mut app,
            failing,
            no_write,
            keys(&[press('r')]),
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "database is locked");
    }

    fn comment(issue: &str, author: Option<&str>, at: i64, body: &str) -> Comment {
        Comment {
            id: format!("c-{at}"),
            issue_id: issue.into(),
            body: body.into(),
            author: author.map(String::from),
            created_at: at,
            deleted_at: None,
        }
    }

    /// `lines`, each padded with spaces to `width`.
    fn padded(lines: &[&str], width: usize) -> Vec<String> {
        lines.iter().map(|l| format!("{l:width$}")).collect()
    }

    fn opened(app: &App) -> Option<&str> {
        app.open().map(IssueId::as_str)
    }

    #[test]
    fn enter_opens_the_selected_issue_and_esc_goes_back_to_the_list_as_it_was() {
        let mut app = App::new(five());
        app.key(press('1'));
        app.key(press('j'));
        assert_eq!(selected(&app), "W-4");
        assert_eq!(app.key(code(KeyCode::Enter)), Action::Reload);
        assert_eq!(opened(&app), Some("id-4"));
        // In the detail, j scrolls, a number filters nothing, and r and q
        // still reload and quit.
        app.key(press('j'));
        app.key(press('0'));
        assert_eq!(app.key(press('r')), Action::Reload);
        assert_eq!(app.key(press('q')), Action::Quit);
        assert_eq!(app.key(code(KeyCode::Esc)), Action::Stay);
        assert_eq!(
            (opened(&app), app.filter, selected(&app)),
            (None, Some(Status::Backlog), "W-4")
        );
        // Esc in the list does nothing; Enter on an empty list neither.
        assert_eq!(app.key(code(KeyCode::Esc)), Action::Stay);
        app.key(press('7'));
        assert_eq!(app.key(code(KeyCode::Enter)), Action::Stay);
        assert_eq!(opened(&app), None);
    }

    #[test]
    fn the_detail_shows_the_fields_the_description_and_the_comments_oldest_first() {
        let mut one = full(
            1,
            Status::InProgress,
            Priority::High,
            "Fix the login redirect",
            Some("tui"),
            Some("austin"),
        );
        one.labels = vec!["bug".into(), "ui".into()];
        one.description = "It loops.\n\nSteps:\n\tlog in".into();
        let mut snapshot = work(vec![one, issue(2, Status::Todo)]);
        snapshot.comments = vec![
            comment("id-1", Some("brutus"), 60, "Seen it\ntoo"),
            comment("id-2", Some("brutus"), 70, "On W-2"),
            comment("id-1", None, 90, "Fixed"),
        ];
        let mut app = App::new(snapshot);
        app.time = |at| format!("{at}s");
        app.key(code(KeyCode::Enter));
        assert_eq!(
            frame(&app, 40, 22),
            padded(
                &[
                    "work (W) · 2 issues",
                    "W-1  Fix the login redirect",
                    "",
                    "Status    InProgress",
                    "Priority  High",
                    "Assignee  austin",
                    "Labels    bug, ui",
                    "",
                    "It loops.",
                    "",
                    "Steps:",
                    " log in",
                    "",
                    "Comments (2)",
                    "",
                    "brutus · 60s",
                    "  Seen it",
                    "  too",
                    "",
                    "anon · 90s",
                    "  Fixed",
                    "j/k: scroll  PgUp/PgDn: page  Esc: back",
                ],
                40
            )
        );

        // An issue with no assignee, labels or description, and only its
        // own comment.
        app.key(code(KeyCode::Esc));
        app.key(press('j'));
        app.key(code(KeyCode::Enter));
        assert_eq!(
            frame(&app, 30, 16)[1..15],
            padded(
                &[
                    "W-2  issue 2",
                    "",
                    "Status    Todo",
                    "Priority  None",
                    "Assignee  none",
                    "Labels    none",
                    "",
                    "No description.",
                    "",
                    "Comments (1)",
                    "",
                    "brutus · 70s",
                    "  On W-2",
                    "",
                ],
                30
            )
        );
    }

    /// An issue whose description is 30 numbered lines: its detail is 39
    /// lines, the last "No comments.".
    fn long() -> App {
        let mut one = issue(1, Status::Todo);
        one.description = (1..=30)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut app = App::new(work(vec![one]));
        app.key(code(KeyCode::Enter));
        app
    }

    #[test]
    fn a_long_detail_scrolls_by_line_by_page_and_to_either_end() {
        let mut app = long();
        // Six lines of detail between the header and the keys.
        let top = |app: &App| frame(app, 20, 8)[1].trim_end().to_string();
        assert_eq!(top(&app), "W-1  issue 1");
        let keys = |app: &mut App, keys: &[KeyEvent]| {
            for k in keys {
                app.key(*k);
            }
            top(app)
        };
        assert_eq!(keys(&mut app, &[press('j')]), "");
        assert_eq!(keys(&mut app, &[code(KeyCode::Down)]), "Status    Todo");
        assert_eq!(
            keys(&mut app, &[press('k'), press('k'), press('k')]),
            "W-1  issue 1"
        );
        // A page is the six lines shown: line 6 is the blank before the
        // description, which starts on line 7.
        assert_eq!(
            keys(&mut app, &[code(KeyCode::PageDown), press('j')]),
            "line 1"
        );
        assert_eq!(keys(&mut app, &[press(' ')]), "line 7");
        assert_eq!(keys(&mut app, &[code(KeyCode::PageUp)]), "line 1");
        // The end puts the last line at the bottom, and j goes no further.
        assert_eq!(keys(&mut app, &[press('G')]), "line 27");
        assert_eq!(frame(&app, 20, 8)[6].trim_end(), "No comments.");
        assert_eq!(
            keys(&mut app, &[press('j'), code(KeyCode::PageDown)]),
            "line 27"
        );
        assert_eq!(keys(&mut app, &[press('b')]), "line 21");
        assert_eq!(keys(&mut app, &[code(KeyCode::Home)]), "W-1  issue 1");
        assert_eq!(keys(&mut app, &[code(KeyCode::End)]), "line 27");

        // Taller, all of it fits: it is drawn from the top, and k moves
        // from there.
        assert_eq!(frame(&app, 20, 41)[1].trim_end(), "W-1  issue 1");
        app.key(press('k'));
        assert_eq!(top(&app), "W-1  issue 1");
    }

    #[test]
    fn a_reload_keeps_the_detail_on_its_issue_or_goes_back_if_it_is_gone() {
        let mut app = App::new(five());
        app.key(press('1'));
        app.key(press('j'));
        app.key(code(KeyCode::Enter));
        // W-4 is now Done, out of the filter: the detail still shows it,
        // and the list's selection moves to the row it was on.
        let mut done = five();
        done.issues[3].status = Status::Done;
        app.reloaded(done.clone());
        assert_eq!(opened(&app), Some("id-4"));
        assert_eq!(frame(&app, 30, 6)[3].trim_end(), "Status    Done");
        app.key(code(KeyCode::Esc));
        assert_eq!(selected(&app), "W-5");
        // W-5 is open, and gone at the next reload: back to the list.
        app.key(code(KeyCode::Enter));
        app.reloaded(work(done.issues.into_iter().take(4).collect()));
        assert_eq!((opened(&app), selected(&app)), (None, "W-2"));
        assert_eq!(
            frame(&app, 30, 4)[3].trim_end(),
            "j/k: move  Enter: open  s: sta"
        );
    }

    /// At every width from 0 to 60, every line of a detail with long words,
    /// wide characters, tabs and indented comments fits the width.
    #[test]
    fn every_line_of_the_detail_fits_its_width() {
        let mut snapshot = four();
        snapshot.issues[0].description =
            "word ".repeat(30) + "\n\tindented\n" + &"x".repeat(70) + "\n日本語のタイトル e\u{301}";
        snapshot.comments = vec![comment(
            "id-1",
            Some(&"a".repeat(50)),
            0,
            "日本語 body\twith a tab",
        )];
        let mut app = App::new(snapshot);
        app.key(code(KeyCode::Enter));
        let issue = app.snapshot.issues[0].clone();
        for width in 0..=60 {
            let lines = app.detail_lines(&issue, width);
            assert!(width < 4 || lines.len() > 12, "{width}: {}", lines.len());
            for line in lines {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(cells(&text) <= width, "width {width}: {text:?}");
                assert!(!text.contains(char::is_control), "{text:?}");
            }
        }
    }

    #[test]
    fn wrap_breaks_at_spaces_and_inside_words_too_long_for_a_line() {
        assert_eq!(wrap("the quick brown fox", 10), ["the quick", "brown fox"]);
        assert_eq!(wrap("the quick brown fox", 9), ["the quick", "brown fox"]);
        assert_eq!(wrap("one two   three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab abcdefghij", 4), ["ab", "abcd", "efgh", "ij"]);
        // A blank line stays; a paragraph's indentation stays; a tab is a
        // space; a carriage return before a newline goes.
        assert_eq!(wrap("a\n\n  b c", 5), ["a", "", "  b c"]);
        assert_eq!(wrap("  abcdefgh", 6), ["  abcd", "efgh"]);
        assert_eq!(wrap("x\ty\r\nz  ", 9), ["x y", "z"]);
        assert_eq!(wrap("", 5), [""]);
        // Wide characters take two cells and are not cut in half.
        assert_eq!(wrap("日本語の文", 5), ["日本", "語の", "文"]);
        assert_eq!(
            wrap("e\u{301}e\u{301}e\u{301}", 2),
            ["e\u{301}e\u{301}", "e\u{301}"]
        );
        assert!(wrap("anything", 0).is_empty());
        // The words come back in order, at every width.
        let text = "Write the importer for Linear exports, with comments and labels";
        for width in 1..=70 {
            let lines = wrap(text, width);
            assert!(
                lines.iter().all(|l| cells(l) <= width),
                "{width}: {lines:?}"
            );
            assert_eq!(
                lines.concat().replace(' ', ""),
                text.replace(' ', ""),
                "{width}"
            );
        }
    }

    #[test]
    fn the_loop_gives_a_reload_the_open_issue() {
        let mut terminal = Terminal::new(TestBackend::new(48, 9)).unwrap();
        let mut app = App::new(five());
        let asked = RefCell::new(Vec::new());
        let reload = |r: &Request| {
            asked
                .borrow_mut()
                .push(r.open.as_ref().map(|id| id.as_str().to_string()));
            Ok(five())
        };
        let pressed = [
            press('j'),
            code(KeyCode::Enter),
            press('r'),
            code(KeyCode::Esc),
            press('r'),
            press('q'),
        ];
        run_app(&mut terminal, &mut app, reload, no_write, keys(&pressed)).unwrap();
        assert_eq!(
            asked.take(),
            [Some("id-2".to_string()), Some("id-2".to_string()), None]
        );
    }

    /// `load` reads the open issue's comments, oldest first, and not those
    /// in the trash.
    #[test]
    fn load_reads_the_open_issue_s_comments() {
        let mut store = Store::open_in_memory().unwrap();
        let issue = store
            .create_issue(
                NewIssue {
                    title: "one".into(),
                    status: Status::Todo,
                    ..Default::default()
                },
                &Default::default(),
            )
            .unwrap();
        let one = lcl_core::models::IssueRef::Number {
            key: None,
            number: 1,
        };
        for body in ["first", "second", "gone"] {
            store
                .add_comment(&one, body.into(), Some("al".into()))
                .unwrap();
        }
        let gone = store.list_comments(issue.id.as_str(), false).unwrap()[2]
            .id
            .clone();
        store.delete_comment(&gone).unwrap();
        let bodies = |open: Option<&IssueId>| -> Vec<String> {
            let request = Request {
                open: open.cloned(),
                search: None,
            };
            load(&store, &request)
                .unwrap()
                .comments
                .into_iter()
                .map(|c| c.body)
                .collect()
        };
        assert_eq!(bodies(Some(&issue.id)), ["first", "second"]);
        assert!(bodies(None).is_empty());
    }

    fn ids(app: &App) -> Vec<&str> {
        app.visible()
            .iter()
            .map(|i| i.identifier.as_str())
            .collect()
    }

    fn issue_ids(ids: &[&str]) -> Vec<IssueId> {
        ids.iter()
            .map(|id| IssueId::from_stored(id.to_string()))
            .collect()
    }

    #[test]
    fn the_prompt_edits_whole_graphemes_and_takes_wide_characters_and_paste() {
        let mut p = Prompt::new("");
        p.key(press('日'));
        p.key(press('本'));
        p.insert("e\u{301}");
        assert_eq!((p.text.as_str(), p.cursor), ("日本e\u{301}", 9));
        // Backspace takes the letter and its accent together.
        p.key(code(KeyCode::Backspace));
        assert_eq!((p.text.as_str(), p.cursor), ("日本", 6));
        p.key(code(KeyCode::Left));
        p.key(press('x'));
        assert_eq!((p.text.as_str(), p.cursor), ("日x本", 4));
        p.key(code(KeyCode::Delete));
        p.key(code(KeyCode::Home));
        p.key(code(KeyCode::Delete));
        assert_eq!((p.text.as_str(), p.cursor), ("x", 0));
        p.key(code(KeyCode::Left));
        p.key(code(KeyCode::End));
        p.key(code(KeyCode::Right));
        assert_eq!(p.cursor, 1);
        // A paste goes in at the cursor, its newlines as spaces.
        p.key(code(KeyCode::Home));
        p.insert("a\nb\r\n");
        assert_eq!((p.text.as_str(), p.cursor), ("a b  x", 5));
        // A chord with Ctrl or Alt types nothing.
        p.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        p.key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(p.text, "a b  x");
        // A character that joins the one after the cursor: the cursor goes
        // past the grapheme they make.
        let mut flag = Prompt::new("🇸");
        flag.key(code(KeyCode::Home));
        flag.insert("🇺");
        assert_eq!((flag.text.as_str(), flag.cursor), ("🇺🇸", 8));
        flag.key(code(KeyCode::Backspace));
        assert_eq!(flag.text, "");
    }

    #[test]
    fn slash_opens_the_prompt_and_esc_leaves_the_list_as_it_was() {
        let mut app = App::new(five());
        app.key(press('1'));
        app.key(press('j'));
        assert_eq!(app.key(press('/')), Action::Stay);
        // At the prompt, q, r and j are typed.
        for c in "qrj".chars() {
            assert_eq!(app.key(press(c)), Action::Stay);
        }
        assert_eq!(frame(&app, 20, 3)[2], "/qrj                ");
        assert_eq!(app.key(code(KeyCode::Esc)), Action::Stay);
        assert!(matches!(app.mode, Mode::List));
        assert_eq!(
            (app.filter, selected(&app), app.search.is_none()),
            (Some(Status::Backlog), "W-4", true)
        );
        // Ctrl-C quits from the prompt too.
        app.key(press('/'));
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
    }

    #[test]
    fn enter_at_the_prompt_searches_and_the_list_shows_the_hits_best_first() {
        let mut app = App::new(four());
        app.key(press('/'));
        app.paste("login redirect");
        assert_eq!(app.key(code(KeyCode::Enter)), Action::Reload);
        let asked = app.request().search.map(|q| q.raw().to_string());
        assert_eq!(asked.as_deref(), Some("login redirect"));
        // The reload brings what the search found, best first, and selects
        // the best.
        let mut found = four();
        found.search = Some(Found {
            query: "login redirect".into(),
            issues: issue_ids(&["id-10", "id-1"]),
        });
        app.reloaded(found);
        assert_eq!(
            frame(&app, 70, 5)[..3],
            [
                "work (W) · 2 of 4 issues · search: login redirect                     ",
                ">> W-10   InProgress  High    Tabs and newlines                       ",
                "   W-1    Done        Urgent  Fix the login red…  tui           austin",
            ]
            .map(String::from)
        );
        // The status filter narrows the hits.
        app.key(press('5'));
        assert_eq!(ids(&app), ["W-1"]);
        assert_eq!(
            app.header(),
            "work (W) · 1 of 4 issues · status: Done · search: login redirect"
        );
        // A reload with the same search keeps the selection.
        app.key(press('0'));
        app.key(press('j'));
        let mut again = four();
        again.search = Some(Found {
            query: "login redirect".into(),
            issues: issue_ids(&["id-10", "id-1"]),
        });
        app.reloaded(again);
        assert_eq!(selected(&app), "W-1");
        // `/` opens the prompt with the search in it; an empty line takes
        // the search away, with no reload.
        app.key(press('/'));
        assert_eq!(frame(&app, 30, 3)[2], "/login redirect               ");
        for _ in 0.."login redirect".len() {
            app.key(code(KeyCode::Backspace));
        }
        assert_eq!(app.key(code(KeyCode::Enter)), Action::Stay);
        assert_eq!(
            (app.header(), selected(&app)),
            ("work (W) · 4 issues".to_string(), "W-1")
        );
        assert!(app.request().search.is_none());
    }

    #[test]
    fn enter_with_nothing_to_search_for_says_so_and_stays_at_the_prompt() {
        let mut app = App::new(five());
        app.key(press('/'));
        app.key(press('-'));
        app.key(press('-'));
        assert_eq!(app.key(code(KeyCode::Enter)), Action::Stay);
        let lines = frame(&app, 60, 3);
        assert_eq!(
            lines[0].trim_end(),
            "the search query \"--\" has no words to search for"
        );
        assert_eq!(lines[2].trim_end(), "/--");
        // The next key takes the message away.
        app.key(code(KeyCode::Backspace));
        assert_eq!(frame(&app, 60, 3)[0].trim_end(), "work (W) · 5 issues");
    }

    #[test]
    fn the_prompt_line_keeps_the_cursor_on_screen() {
        let mut app = App::new(five());
        app.key(press('/'));
        app.paste("日本語 search");
        let mut terminal = Terminal::new(TestBackend::new(10, 3)).unwrap();
        // The last line as it reads: the cell a wide character spills
        // into is left out.
        let mut drawn = |app: &App| {
            terminal.draw(|f| app.draw(f)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            let buf = terminal.backend().buffer();
            let (mut line, mut x) = (String::new(), 0);
            while x < buf.area.width {
                let symbol = buf[(x, 2)].symbol();
                line.push_str(symbol);
                x += u16::try_from(cells(symbol).max(1)).unwrap();
            }
            (line, (cursor.x, cursor.y))
        };
        // `/日本語 search` is 14 cells: its first 5 are left out, so the
        // cursor gets the last cell.
        assert_eq!(drawn(&app), ("語 search ".to_string(), (9, 2)));
        // At the start the whole line is drawn from `/`, cut at the edge.
        app.key(code(KeyCode::Home));
        assert_eq!(drawn(&app), ("/日本語 se".to_string(), (1, 2)));
        app.key(code(KeyCode::Right));
        assert_eq!(drawn(&app).1, (3, 2));
        app.key(code(KeyCode::Right));
        assert_eq!(drawn(&app).1, (5, 2));
    }

    #[test]
    fn p_goes_through_the_projects_by_name_then_back_to_all() {
        let mut app = App::new(four());
        app.key(press('j'));
        app.key(press('p'));
        assert_eq!(
            (app.header(), ids(&app), app.row()),
            (
                "work (W) · 1 of 4 issues · project: a very long project name".to_string(),
                vec!["W-112"],
                0
            )
        );
        app.key(press('p'));
        assert_eq!(ids(&app), ["W-2"]);
        app.key(press('p'));
        assert_eq!(ids(&app), ["W-1"]);
        // It combines with the status filter.
        app.key(press('2'));
        assert_eq!(
            (app.header(), ids(&app).len()),
            (
                "work (W) · 0 of 4 issues · status: Todo · project: tui".to_string(),
                0
            )
        );
        app.key(press('0'));
        // A reload brings the project's new name.
        let mut renamed = four();
        renamed.issues[0].project = Some("terminal".into());
        app.reloaded(renamed);
        assert!(
            app.header().ends_with("project: terminal"),
            "{}",
            app.header()
        );
        // After the last project, every project again.
        app.key(press('p'));
        assert_eq!(app.header(), "work (W) · 4 issues");
        // A workspace with no projects: p does nothing.
        let mut plain = App::new(five());
        plain.key(press('p'));
        assert_eq!(plain.header(), "work (W) · 5 issues");
    }

    /// `load` searches as `lcl search` does: an issue's own text or its
    /// comments, and the issue a query names first.
    #[test]
    fn load_searches_as_lcl_search_does() {
        let mut store = Store::open_in_memory().unwrap();
        store.set_meta(&[("name", "demo"), ("key", "D")]).unwrap();
        for title in ["Fix the login redirect", "Write the docs", "Unrelated"] {
            store
                .create_issue(
                    NewIssue {
                        title: title.into(),
                        status: Status::Todo,
                        ..Default::default()
                    },
                    &Default::default(),
                )
                .unwrap();
        }
        let two = lcl_core::models::IssueRef::Number {
            key: None,
            number: 2,
        };
        store
            .add_comment(&two, "the login page is slow".into(), None)
            .unwrap();
        let search = |q: &str| -> Vec<String> {
            let request = Request {
                open: None,
                search: Some(SearchQuery::parse(q).unwrap()),
            };
            let snapshot = load(&store, &request).unwrap();
            let found = snapshot.search.unwrap();
            assert_eq!(found.query, q);
            found
                .issues
                .iter()
                .map(|id| {
                    let issue = snapshot.issues.iter().find(|i| &i.id == id).unwrap();
                    issue.identifier.clone()
                })
                .collect()
        };
        let mut login = search("login");
        login.sort();
        assert_eq!(login, ["D-1", "D-2"]);
        assert_eq!(search("D-3")[0], "D-3");
        assert!(search("nowhere").is_empty());
    }

    #[test]
    fn the_loop_puts_a_paste_in_the_prompt_and_reloads_with_its_search() {
        let mut terminal = Terminal::new(TestBackend::new(48, 9)).unwrap();
        let mut app = App::new(five());
        let asked = RefCell::new(Vec::new());
        let reload = |r: &Request| {
            asked
                .borrow_mut()
                .push(r.search.as_ref().map(|q| q.raw().to_string()));
            Ok(five())
        };
        let mut inputs = [
            Input::Key(press('/')),
            Input::Paste("login\n".into()),
            Input::Key(code(KeyCode::Enter)),
            Input::Paste("ignored".into()),
            Input::Key(press('q')),
        ]
        .into_iter();
        let next = move || {
            inputs
                .next()
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("out of input"))
        };
        run_app(&mut terminal, &mut app, reload, no_write, next).unwrap();
        assert_eq!(asked.take(), [Some("login ".to_string())]);
    }

    fn id(id: &str) -> IssueId {
        IssueId::from_stored(id.to_string())
    }

    #[test]
    fn s_and_bang_choose_a_value_and_esc_writes_nothing() {
        let mut app = App::new(four());
        assert_eq!(app.key(press('s')), Action::Stay);
        // A box over the list, at the issue's status.
        assert_eq!(
            frame(&app, 44, 12)[1..],
            [
                ">> W-1    Don┌ W-1 status ────┐ix the login…",
                "   W-2    Bac│   1  Backlog   │rite the imp…",
                "   W-10   InP│   2  Todo      │abs and newl…",
                "   W-112  Tod│   3  InProgress│hort         ",
                "             │   4  InReview  │             ",
                "             │>> 5  Done (now)│             ",
                "             │   6  Canceled  │             ",
                "             │   7  Duplicate │             ",
                "             └────────────────┘             ",
                "                                            ",
                "j/k: move  Enter or 1-7: set  Esc: cancel   ",
            ]
            .map(String::from)
        );
        // Esc: back to the list, nothing to write.
        assert_eq!(app.key(code(KeyCode::Esc)), Action::Stay);
        assert!(matches!(app.mode, Mode::List));
        // ! opens at the issue's priority, Urgent; j moves to High.
        app.key(press('!'));
        app.key(press('j'));
        assert_eq!(
            app.key(code(KeyCode::Enter)),
            Action::Write(Edit::Priority(id("id-1"), Priority::High))
        );
        assert!(matches!(app.mode, Mode::List));
        // A value's number sets it at once: statuses 1-7, priorities 0-4.
        app.key(press('s'));
        assert_eq!(
            app.key(press('2')),
            Action::Write(Edit::Status(id("id-1"), Status::Todo))
        );
        app.key(press('!'));
        assert_eq!(
            app.key(press('0')),
            Action::Write(Edit::Priority(id("id-1"), Priority::None))
        );
        // The value the issue has: nothing to write.
        app.key(press('s'));
        assert_eq!(app.key(press('5')), Action::Stay);
        assert!(matches!(app.mode, Mode::List));
        // A number with no value, and j past the last, do nothing.
        app.key(press('!'));
        assert_eq!(app.key(press('7')), Action::Stay);
        for _ in 0..9 {
            app.key(press('j'));
        }
        assert_eq!(
            app.key(code(KeyCode::Enter)),
            Action::Write(Edit::Priority(id("id-1"), Priority::Low))
        );
        // In a choice, r is not a reload, and q still quits.
        app.key(press('s'));
        assert_eq!(app.key(press('r')), Action::Stay);
        assert_eq!(app.key(press('q')), Action::Quit);
        // With nothing selected, s and ! open nothing.
        let mut empty = App::new(work(vec![]));
        empty.key(press('s'));
        empty.key(press('!'));
        assert!(matches!(empty.mode, Mode::List));
    }

    #[test]
    fn a_choice_closes_if_a_reload_finds_its_issue_gone() {
        let mut app = App::new(five());
        app.key(press('s'));
        app.reloaded(work(five().issues.into_iter().skip(1).collect()));
        assert!(matches!(app.mode, Mode::List));
        assert_eq!(selected(&app), "W-2");
    }

    /// Workspace demo (D) with D-1, D-2 and D-3, all Backlog, written by al.
    fn three() -> Store {
        let mut store = Store::open_in_memory().unwrap();
        store.set_meta(&[("name", "demo"), ("key", "D")]).unwrap();
        store.set_author(Some("al".into()));
        for title in ["one", "two", "three"] {
            store
                .create_issue(
                    NewIssue {
                        title: title.into(),
                        ..Default::default()
                    },
                    &Default::default(),
                )
                .unwrap();
        }
        store
    }

    /// The loop as `run` wires it, on `store`, with these keys.
    fn run_on(store: &RefCell<Store>, app: &mut App, pressed: &[KeyEvent]) -> Result<()> {
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        run_app(
            &mut terminal,
            app,
            |r| load(&store.borrow(), r),
            |e| write(&mut store.borrow_mut(), e),
            keys(pressed),
        )
    }

    #[test]
    fn a_write_goes_through_update_issue_and_the_list_keeps_its_issue() {
        let store = RefCell::new(three());
        let mut app = App::new(load(&store.borrow(), &Request::default()).unwrap());
        // D-2, In Progress, then Urgent.
        let pressed = [
            press('j'),
            press('s'),
            press('3'),
            press('!'),
            press('1'),
            press('q'),
        ];
        run_on(&store, &mut app, &pressed).unwrap();
        let issue = app.selected_issue().unwrap();
        assert_eq!(
            (issue.identifier.as_str(), issue.status, issue.priority),
            ("D-2", Status::InProgress, Priority::Urgent)
        );
        // Each write is one entry in the change log, by the store's author.
        let two = IssueRef::Number {
            key: None,
            number: 2,
        };
        let history = store.borrow().issue_history(&two).unwrap();
        let authors: Vec<Option<&str>> = history.iter().map(|h| h.author.as_deref()).collect();
        assert_eq!(authors, [Some("al"); 3]);
        assert_eq!(history[1].changes.len(), 2, "status and started_at");
        assert_eq!(history[2].changes.len(), 1, "priority");
        // Under a status filter the issue leaves the list; the selection
        // goes to the row it was on.
        run_on(
            &store,
            &mut app,
            &[press('3'), press('s'), press('5'), press('q')],
        )
        .unwrap();
        assert_eq!(app.visible().len(), 0);
    }

    #[test]
    fn a_failed_write_says_why_and_changes_nothing() {
        let store = RefCell::new(three());
        let mut app = App::new(load(&store.borrow(), &Request::default()).unwrap());
        // Another process puts D-3 in the trash.
        let three_ref = IssueRef::Number {
            key: None,
            number: 3,
        };
        store.borrow_mut().delete_issue(&three_ref).unwrap();
        let out = run_on(&store, &mut app, &[press('s'), press('5')]).unwrap_err();
        assert_eq!(out.to_string(), "out of keys");
        // The list is as it was read: D-3 still in it, still Backlog.
        let issue = app.selected_issue().unwrap();
        assert_eq!(
            (issue.identifier.as_str(), issue.status),
            ("D-3", Status::Backlog)
        );
        assert_eq!(
            frame(&app, 90, 4)[3].trim_end(),
            "Not written: D-3 is in the trash; restore it with: lcl issue restore D-3"
        );
        // The next key takes the message away.
        app.key(press('j'));
        assert!(frame(&app, 60, 4)[3].starts_with("j/k: move"));
        // A failed write leaves the loop running: it went on to the q.
        let reloads = RefCell::new(0);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        run_app(
            &mut terminal,
            &mut app,
            |_: &Request| {
                *reloads.borrow_mut() += 1;
                Ok(work(vec![]))
            },
            |_: &Edit| Err(anyhow::anyhow!("database is locked")),
            keys(&[press('!'), press('1'), press('j'), press('q')]),
        )
        .unwrap();
        assert_eq!(*reloads.borrow(), 0);
    }

    /// `leave` runs once `enter` has, on every way out of `body`.
    #[test]
    fn the_terminal_is_restored_on_every_way_out() {
        let log = RefCell::new(Vec::<&str>::new());
        let enter = || {
            log.borrow_mut().push("enter");
            Ok(())
        };
        let leave = || log.borrow_mut().push("leave");

        let out = guarded(enter, leave, || Ok(7));
        assert_eq!((out.unwrap(), log.take()), (7, vec!["enter", "leave"]));

        let out: Result<()> = guarded(enter, leave, || Err(anyhow::anyhow!("draw failed")));
        assert_eq!(
            (out.unwrap_err().to_string(), log.take()),
            ("draw failed".to_string(), vec!["enter", "leave"])
        );

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            guarded(enter, leave, || -> Result<()> { panic!("bug") })
        }));
        assert!(panicked.is_err());
        assert_eq!(log.take(), vec!["enter", "leave"]);

        let out: Result<()> = guarded(
            || Err(anyhow::anyhow!("not a terminal")),
            leave,
            || panic!("body ran"),
        );
        assert_eq!(
            (out.unwrap_err().to_string(), log.take()),
            ("not a terminal".to_string(), vec![])
        );
    }
}
