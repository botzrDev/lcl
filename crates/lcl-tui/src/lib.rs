use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use lcl_core::db::{IssueFilter, IssueSort, Page, Store};
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::io::stdout;
use std::time::Duration;

/// Runs the TUI on an open workspace; the caller resolves which one.
pub fn run(store: Store) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut selected: usize = 0;
    let mut status_filter: Option<&str> = None;
    let issues = store.list_issues(&IssueFilter::default(), IssueSort::Number, Page::ALL).map(|list| list.issues).unwrap_or_default();

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(2)]).split(f.area());
            let title = Paragraph::new("lcl TUI  (q quit, j/k nav, Enter view)").block(Block::default().borders(Borders::ALL).title("Linear-lite"));
            f.render_widget(title, chunks[0]);

            let filtered: Vec<&lcl_core::models::Issue> = issues.iter().filter(|i| status_filter.is_none() || i.status.as_str() == status_filter.unwrap()).collect();
            let items: Vec<ListItem> = filtered.iter().map(|i| {
                ListItem::new(format!("{} [{}] {} - {}", i.identifier, i.status, i.priority.label(), i.title))
            }).collect();
            let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Issues")).highlight_style(Style::default().bg(Color::DarkGray)).highlight_symbol(">> ");
            let mut state = ListState::default();
            state.select(Some(selected.min(filtered.len().saturating_sub(1))));
            f.render_stateful_widget(list, chunks[1], &mut state);

            let help = Paragraph::new("j/k: move  q: quit  1-7: filter status  0: all").block(Block::default().borders(Borders::ALL));
            f.render_widget(help, chunks[2]);
        })?;

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('j') | KeyCode::Down => { if selected + 1 < issues.len() { selected += 1; } }
                KeyCode::Char('k') | KeyCode::Up => { selected = selected.saturating_sub(1); }
                KeyCode::Char('0') => status_filter = None,
                KeyCode::Char('1') => status_filter = Some("Backlog"),
                KeyCode::Char('2') => status_filter = Some("Todo"),
                KeyCode::Char('3') => status_filter = Some("InProgress"),
                KeyCode::Char('4') => status_filter = Some("InReview"),
                KeyCode::Char('5') => status_filter = Some("Done"),
                KeyCode::Char('6') => status_filter = Some("Canceled"),
                KeyCode::Char('7') => status_filter = Some("Duplicate"),
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
