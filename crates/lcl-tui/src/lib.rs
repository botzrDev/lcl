//! A read-only terminal view of a workspace's issues: a list, and the
//! detail of one issue.
//!
//! The state and its key handling (`App`) are pure, and drawing takes any
//! ratatui backend, so both are tested without a terminal. `run` puts the
//! terminal in raw mode on the alternate screen, and `guarded` puts it back
//! on every way out.

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use lcl_core::db::{IssueFilter, IssueSort, Page, Store};
use lcl_core::models::{Comment, Issue, IssueId, Status};
use ratatui::buffer::CellWidth;
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::borrow::Cow;
use std::cell::Cell;
use std::io::{IsTerminal, stdout};
use std::time::Duration;

const LIST_HELP: &str = "j/k: move  Enter: open  r: reload  q: quit  1-7: filter status  0: all";
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
/// issue in list order, and the comments of the issue open in the detail.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub workspace: String,
    pub key: String,
    pub issues: Vec<Issue>,
    /// The comments of the issue that was open (`App::open`) when this was
    /// read, oldest first; none while the list is shown.
    pub comments: Vec<Comment>,
}

/// What the loop does after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Read the issues again, and the open issue's comments (see
    /// `App::open` and `App::reloaded`).
    Reload,
    Quit,
}

/// What the screen shows. The detail's state exists only while it is shown.
enum Mode {
    List,
    Detail(Detail),
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

/// The workspace as last read, the status filter, the selected issue, and
/// what the screen shows: the list, or the detail of one issue.
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
    selected: Option<IssueId>,
    mode: Mode,
    /// How a comment's time is shown; local time, but for the tests.
    time: fn(i64) -> String,
}

impl App {
    pub fn new(snapshot: Snapshot) -> App {
        let mut app = App {
            widths: Widths::of(&snapshot.issues),
            snapshot,
            filter: None,
            selected: None,
            mode: Mode::List,
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
            Mode::List => None,
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

    /// The issues the filter lets through, in list order.
    pub fn visible(&self) -> Vec<&Issue> {
        self.snapshot
            .issues
            .iter()
            .filter(|i| self.filter.is_none_or(|s| i.status == s))
            .collect()
    }

    /// Handles one key. Only presses count. Everywhere, q and Ctrl-C quit
    /// and r asks for a reload. In the list, j/k (or the arrows) move
    /// within the filtered list, 1-7 filter by status and 0 clears the
    /// filter (each selecting the first row), and Enter opens the selected
    /// issue's detail, asking for a reload to read its comments. In the
    /// detail, keys scroll (`Detail::scroll`), and Esc goes back to the
    /// list, as it was.
    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::Stay;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Action::Quit;
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('r') => return Action::Reload,
            _ => {}
        }
        match &mut self.mode {
            Mode::List => self.list_key(key.code),
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
            _ => Action::Stay,
        }
    }

    /// Takes a fresh read of the workspace, keeping the filter and the selected
    /// issue, whatever row it is on now. If that issue is no longer in the
    /// filtered list, the selection goes to the row it was on (else the last).
    /// The detail stays on its issue, or goes back to the list if the
    /// workspace no longer has it (deleted, or purged).
    pub fn reloaded(&mut self, snapshot: Snapshot) {
        let row = self.row();
        self.widths = Widths::of(&snapshot.issues);
        self.snapshot = snapshot;
        if !self
            .visible()
            .iter()
            .any(|i| Some(&i.id) == self.selected.as_ref())
        {
            self.select_row(row);
        }
        if let Mode::Detail(d) = &self.mode
            && !self.snapshot.issues.iter().any(|i| i.id == d.id)
        {
            self.mode = Mode::List;
        }
    }

    /// The header line: the workspace's name, with its key in brackets if it
    /// has one; how many issues it has; and under a filter, how many of them
    /// are listed and what the filter is. `demo (D) · 2 of 5 issues · status: Done`.
    fn header(&self) -> String {
        let mut out = self.snapshot.workspace.clone();
        if !self.snapshot.key.is_empty() {
            out.push_str(&format!(" ({})", self.snapshot.key));
        }
        let total = self.snapshot.issues.len();
        let issues = if total == 1 { "issue" } else { "issues" };
        match self.filter {
            None => out.push_str(&format!(" · {total} {issues}")),
            Some(status) => out.push_str(&format!(
                " · {} of {total} {issues} · status: {status}",
                self.visible().len()
            )),
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
        let help = match &self.mode {
            Mode::List => {
                self.draw_list(f, chunks[1]);
                LIST_HELP
            }
            Mode::Detail(d) => {
                self.draw_detail(f, chunks[1], d);
                DETAIL_HELP
            }
        };
        f.render_widget(
            Paragraph::new(help).style(Style::default().add_modifier(Modifier::DIM)),
            chunks[2],
        );
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
    let mut app = App::new(load(&store, None)?);
    // Said here, in words: raw mode on something that isn't a terminal
    // fails with the system's "No such device or address".
    if !(std::io::stdin().is_terminal() && stdout().is_terminal()) {
        return Err(lcl_core::error::LclError::NoTerminal.into());
    }
    guarded(enter_terminal, leave_terminal, || {
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        run_app(&mut terminal, &mut app, |open| load(&store, open), next_key)
    })
}

/// The workspace's name and key, every issue by number, and the comments of
/// issue `open`, if one is open, oldest first. Read-only.
fn load(store: &Store, open: Option<&IssueId>) -> Result<Snapshot> {
    Ok(Snapshot {
        workspace: store.get_meta("name")?.unwrap_or_default(),
        key: store.get_meta("key")?.unwrap_or_default(),
        issues: store
            .list_issues(&IssueFilter::default(), IssueSort::Number, Page::ALL)?
            .issues,
        comments: match open {
            Some(id) => store.list_comments(id.as_str(), false)?,
            None => Vec::new(),
        },
    })
}

/// The next key event, or None if none comes within a quarter second.
fn next_key() -> Result<Option<KeyEvent>> {
    if event::poll(Duration::from_millis(250))?
        && let Event::Key(key) = event::read()?
    {
        return Ok(Some(key));
    }
    Ok(None)
}

/// Draws, then acts on the next key, until a quit. A reload is given the
/// open issue, whose comments it reads. An error drawing, reading a key or
/// reloading ends the loop with that error.
fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut reload: impl FnMut(Option<&IssueId>) -> Result<Snapshot>,
    mut next_key: impl FnMut() -> Result<Option<KeyEvent>>,
) -> Result<()>
where
    B::Error: Send + Sync + 'static,
{
    loop {
        terminal.draw(|f| app.draw(f))?;
        let Some(key) = next_key()? else { continue };
        match app.key(key) {
            Action::Quit => return Ok(()),
            Action::Reload => {
                let snapshot = reload(app.open())?;
                app.reloaded(snapshot);
            }
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

/// Raw mode, then the alternate screen; raw mode is undone if the second fails.
fn enter_terminal() -> Result<()> {
    enable_raw_mode()?;
    if let Err(e) = stdout().execute(EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    Ok(())
}

/// Leaves the alternate screen and raw mode. Best effort: on the way out
/// there is nowhere to report a failure.
fn leave_terminal() {
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

    /// Every sequence of up to five keys and reloads, from three starting
    /// lists. After each one: the selection is None exactly when the filtered
    /// list is empty, else an issue in it; a reload that keeps the selected
    /// issue in the filtered list leaves it selected; and an open issue is
    /// one the workspace has.
    #[test]
    fn the_selection_is_always_an_issue_in_the_filtered_list() {
        let reloads = [
            work(vec![]),
            five(),
            numbered(&[6, 5, 4, 3, 2, 1]),
            work(five().issues.into_iter().skip(2).collect()),
        ];
        let keys = [
            press('j'),
            press('k'),
            press('1'),
            press('5'),
            press('7'),
            press('0'),
            code(KeyCode::Enter),
            code(KeyCode::Esc),
        ];
        let ops = keys.len() + reloads.len();
        let apply = |app: &mut App, op: usize| {
            if op < keys.len() {
                app.key(keys[op]);
            } else {
                let before = app.selected.clone();
                app.reloaded(reloads[op - keys.len()].clone());
                if before.is_some() && app.visible().iter().any(|i| Some(&i.id) == before.as_ref())
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
        };
        let mut sequences = 0;
        for start in [work(vec![]), five(), numbered(&[1])] {
            for len in 0..=5u32 {
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
        // 1 + 12 + 12^2 + ... + 12^5 sequences from each start.
        assert_eq!(sequences, 3 * 271_453);
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
                "j/k: move  Enter: open  r: reload  q: quit  1-7: filter stat",
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
                "j/k: move  Enter: open  r: reload  q: quit  1-7: filter stat",
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
                "j/k: move  Enter: open  r: reload  q: quit  1-7: filter status  0: all",
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
        let empty = load(&store, None).unwrap();
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
        let snapshot = load(&store, None).unwrap();
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
            load(store, None)
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

    /// Keys from a list, then an error if the loop asks for more.
    fn keys(list: &[KeyEvent]) -> impl FnMut() -> Result<Option<KeyEvent>> + '_ {
        let mut it = list.iter();
        move || {
            it.next()
                .map(|k| Some(*k))
                .ok_or_else(|| anyhow::anyhow!("out of keys"))
        }
    }

    #[test]
    fn the_loop_quits_reloads_and_ends_with_a_reload_s_error() {
        let mut terminal = Terminal::new(TestBackend::new(48, 9)).unwrap();
        let mut app = App::new(five());
        let reloads = RefCell::new(0);
        let reload = |_: Option<&IssueId>| {
            *reloads.borrow_mut() += 1;
            Ok(five())
        };
        run_app(
            &mut terminal,
            &mut app,
            reload,
            keys(&[press('r'), press('j'), press('q')]),
        )
        .unwrap();
        assert_eq!((*reloads.borrow(), app.row()), (1, 1));

        let failing = |_: Option<&IssueId>| Err(anyhow::anyhow!("database is locked"));
        let e = run_app(&mut terminal, &mut app, failing, keys(&[press('r')])).unwrap_err();
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
            "j/k: move  Enter: open  r: rel"
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
        let reload = |open: Option<&IssueId>| {
            asked
                .borrow_mut()
                .push(open.map(|id| id.as_str().to_string()));
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
        run_app(&mut terminal, &mut app, reload, keys(&pressed)).unwrap();
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
        let bodies = |open| -> Vec<String> {
            load(&store, open)
                .unwrap()
                .comments
                .into_iter()
                .map(|c| c.body)
                .collect()
        };
        assert_eq!(bodies(Some(&issue.id)), ["first", "second"]);
        assert!(bodies(None).is_empty());
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
