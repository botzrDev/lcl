//! A read-only terminal view of a workspace's issues.
//!
//! The state and its key handling (`App`) are pure, and drawing takes any
//! ratatui backend, so both are tested without a terminal. `run` puts the
//! terminal in raw mode on the alternate screen, and `guarded` puts it back
//! on every way out.

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use lcl_core::db::{IssueFilter, IssueSort, Page, Store};
use lcl_core::models::{Issue, Status};
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::io::stdout;
use std::time::Duration;

const TITLE: &str = "lcl TUI  (q quit, j/k nav, r reload)";
const HELP: &str = "j/k: move  r: reload  q: quit  1-7: filter status  0: all";

/// What the loop does after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Read the issues again (see `App::reloaded`).
    Reload,
    Quit,
}

/// The issues as last read, the status filter, and the selected issue.
///
/// The selection is the issue's UUID, not its row, so it stays on the same
/// issue when a reload moves the rows. It is `None` exactly when the
/// filtered list is empty; otherwise it names an issue in that list.
pub struct App {
    issues: Vec<Issue>,
    filter: Option<Status>,
    selected: Option<String>,
}

impl App {
    pub fn new(issues: Vec<Issue>) -> App {
        let mut app = App { issues, filter: None, selected: None };
        app.select_row(0);
        app
    }

    /// The selected issue's row in the filtered list; 0 when it is empty.
    pub fn row(&self) -> usize {
        self.selected.as_ref().and_then(|id| self.visible().iter().position(|i| &i.id == id)).unwrap_or(0)
    }

    /// Selects the issue on `row` of the filtered list, or on its last row
    /// if `row` is past it; nothing when the list is empty.
    fn select_row(&mut self, row: usize) {
        let visible = self.visible();
        let id = visible.get(row.min(visible.len().saturating_sub(1))).map(|i| i.id.clone());
        self.selected = id;
    }

    /// The issues the filter lets through, in list order.
    pub fn visible(&self) -> Vec<&Issue> {
        self.issues.iter().filter(|i| self.filter.is_none_or(|s| i.status == s)).collect()
    }

    /// Handles one key. q and Ctrl-C quit, j/k (or the arrows) move within
    /// the filtered list, 1-7 filter by status and 0 clears the filter (each
    /// selecting the first row), r asks for a reload. Only presses count.
    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::Stay;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('r') => Action::Reload,
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

    /// Takes a fresh read of the issues, keeping the filter and the selected
    /// issue, whatever row it is on now. If that issue is no longer in the
    /// filtered list, the selection goes to the row it was on (else the last).
    pub fn reloaded(&mut self, issues: Vec<Issue>) {
        let row = self.row();
        self.issues = issues;
        if !self.visible().iter().any(|i| Some(&i.id) == self.selected.as_ref()) {
            self.select_row(row);
        }
    }

    pub fn draw(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(3)])
            .split(f.area());
        f.render_widget(Paragraph::new(TITLE).block(Block::default().borders(Borders::ALL).title("Linear-lite")), chunks[0]);

        let items: Vec<ListItem> = self
            .visible()
            .iter()
            .map(|i| ListItem::new(format!("{} [{}] {} - {}", i.identifier, i.status, i.priority.label(), i.title)))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Issues"))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol(">> ");
        let mut state = ListState::default();
        state.select(Some(self.row()));
        f.render_stateful_widget(list, chunks[1], &mut state);

        f.render_widget(Paragraph::new(HELP).block(Block::default().borders(Borders::ALL)), chunks[2]);
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
    let mut app = App::new(load(&store)?);
    guarded(enter_terminal, leave_terminal, || {
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        run_app(&mut terminal, &mut app, || load(&store), next_key)
    })
}

/// Every issue, by number. Read-only.
fn load(store: &Store) -> Result<Vec<Issue>> {
    Ok(store.list_issues(&IssueFilter::default(), IssueSort::Number, Page::ALL)?.issues)
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

/// Draws, then acts on the next key, until a quit. An error drawing,
/// reading a key or reloading ends the loop with that error.
fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut reload: impl FnMut() -> Result<Vec<Issue>>,
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
            Action::Reload => app.reloaded(reload()?),
            Action::Stay => {}
        }
    }
}

/// Runs `body` after `enter`, and `leave` after it however it ends: by
/// returning, with an error, or in a panic. If `enter` fails, neither runs
/// (`enter` undoes its own part way).
fn guarded<T>(enter: impl FnOnce() -> Result<()>, leave: impl FnOnce(), body: impl FnOnce() -> Result<T>) -> Result<T> {
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
    use lcl_core::models::Priority;
    use ratatui::backend::TestBackend;
    use std::cell::RefCell;

    fn issue(number: i64, status: Status) -> Issue {
        Issue {
            id: format!("id-{number}"), number, identifier: format!("W-{number}"), title: format!("issue {number}"), description: String::new(),
            status, priority: Priority::None, assignee: None, parent_id: None, parent_identifier: None, project_id: None, project: None,
            milestone_id: None, milestone: None, r#ref: None, creator: None, estimate: None, due_date: None, labels: vec![], cycles: vec![],
            created_at: 0, updated_at: 0, completed_at: None, started_at: None, canceled_at: None,
        }
    }

    /// W-1 Done, W-2 Backlog, W-3 Done, W-4 Backlog, W-5 Backlog.
    fn five() -> Vec<Issue> {
        [Status::Done, Status::Backlog, Status::Done, Status::Backlog, Status::Backlog].into_iter().enumerate().map(|(n, s)| issue(n as i64 + 1, s)).collect()
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
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
        assert_eq!((app.visible().len(), app.row(), selected(&app)), (2, 1, "W-3"));
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
        assert_eq!((app.visible().len(), app.row(), &app.selected), (0, 0, &None));
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
        app.reloaded(five().into_iter().take(4).collect());
        assert_eq!((app.visible().len(), selected(&app)), (2, "W-4"));
    }

    /// Issues `numbers`, in that order, all Backlog.
    fn numbered(numbers: &[i64]) -> Vec<Issue> {
        numbers.iter().map(|n| issue(*n, Status::Backlog)).collect()
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
        app.reloaded(vec![]);
        assert_eq!((app.row(), &app.selected), (0, &None));
        // From an empty list, a reload selects the first row.
        app.reloaded(numbered(&[2, 1]));
        assert_eq!(selected(&app), "W-2");
    }

    /// Every sequence of up to five keys and reloads, from three starting
    /// lists. After each one: the selection is None exactly when the filtered
    /// list is empty, else an issue in it; and a reload that keeps the
    /// selected issue in the filtered list leaves it selected.
    #[test]
    fn the_selection_is_always_an_issue_in_the_filtered_list() {
        let reloads = [vec![], five(), numbered(&[6, 5, 4, 3, 2, 1]), five().into_iter().skip(2).collect()];
        let keys = ['j', 'k', '1', '5', '7', '0'];
        let ops = keys.len() + reloads.len();
        let apply = |app: &mut App, op: usize| {
            if op < keys.len() {
                app.key(press(keys[op]));
            } else {
                let before = app.selected.clone();
                app.reloaded(reloads[op - keys.len()].clone());
                if before.is_some() && app.visible().iter().any(|i| Some(&i.id) == before.as_ref()) {
                    assert_eq!(app.selected, before, "a reload moved the selection off an issue still listed");
                }
            }
            let visible = app.visible();
            match &app.selected {
                None => assert!(visible.is_empty(), "nothing selected in a list of {}", visible.len()),
                Some(id) => assert!(visible.iter().any(|i| &i.id == id), "{id} is selected but not listed"),
            }
        };
        let mut sequences = 0;
        for start in [vec![], five(), numbered(&[1])] {
            for len in 0..=5u32 {
                for mut n in 0..ops.pow(len) {
                    let mut app = App::new(start.clone());
                    assert_eq!(app.selected.is_none(), start.is_empty());
                    for _ in 0..len {
                        apply(&mut app, n % ops);
                        n /= ops;
                    }
                    sequences += 1;
                }
            }
        }
        assert_eq!(sequences, 3 * 111_111);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = App::new(five());
        assert_eq!(app.key(press('q')), Action::Quit);
        assert_eq!(app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Action::Quit);
        // c alone, and a key release, do nothing.
        assert_eq!(app.key(press('c')), Action::Stay);
        let mut release = press('q');
        release.kind = KeyEventKind::Release;
        assert_eq!(app.key(release), Action::Stay);
    }

    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect()
    }

    #[test]
    fn a_frame_shows_the_title_and_the_filtered_rows() {
        let mut app = App::new(five());
        app.key(press('5'));
        app.key(press('j'));
        let mut terminal = Terminal::new(TestBackend::new(62, 10)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert_eq!(
            lines(&terminal),
            [
                "┌Linear-lite─────────────────────────────────────────────────┐",
                "│lcl TUI  (q quit, j/k nav, r reload)                        │",
                "└────────────────────────────────────────────────────────────┘",
                "┌Issues──────────────────────────────────────────────────────┐",
                "│   W-1 [Done] None - issue 1                                │",
                "│>> W-3 [Done] None - issue 3                                │",
                "└────────────────────────────────────────────────────────────┘",
                "┌────────────────────────────────────────────────────────────┐",
                "│j/k: move  r: reload  q: quit  1-7: filter status  0: all   │",
                "└────────────────────────────────────────────────────────────┘",
            ]
            .map(String::from)
        );
    }

    /// Keys from a list, then an error if the loop asks for more.
    fn keys(list: &[KeyEvent]) -> impl FnMut() -> Result<Option<KeyEvent>> + '_ {
        let mut it = list.iter();
        move || it.next().map(|k| Some(*k)).ok_or_else(|| anyhow::anyhow!("out of keys"))
    }

    #[test]
    fn the_loop_quits_reloads_and_ends_with_a_reload_s_error() {
        let mut terminal = Terminal::new(TestBackend::new(48, 9)).unwrap();
        let mut app = App::new(five());
        let reloads = RefCell::new(0);
        let reload = || {
            *reloads.borrow_mut() += 1;
            Ok(five())
        };
        run_app(&mut terminal, &mut app, reload, keys(&[press('r'), press('j'), press('q')])).unwrap();
        assert_eq!((*reloads.borrow(), app.row()), (1, 1));

        let failing = || Err(anyhow::anyhow!("database is locked"));
        let e = run_app(&mut terminal, &mut app, failing, keys(&[press('r')])).unwrap_err();
        assert_eq!(e.to_string(), "database is locked");
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
        assert_eq!((out.unwrap_err().to_string(), log.take()), ("draw failed".to_string(), vec!["enter", "leave"]));

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guarded(enter, leave, || -> Result<()> { panic!("bug") })));
        assert!(panicked.is_err());
        assert_eq!(log.take(), vec!["enter", "leave"]);

        let out: Result<()> = guarded(|| Err(anyhow::anyhow!("not a terminal")), leave, || panic!("body ran"));
        assert_eq!((out.unwrap_err().to_string(), log.take()), ("not a terminal".to_string(), vec![]));
    }
}
