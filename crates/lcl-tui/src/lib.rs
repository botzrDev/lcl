use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use lcl_core::db::Store;
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::io::stdout;
use std::time::Duration;

pub fn run(project: Option<String>) -> Result<()> {
    // resolve store path similar to cli
    let store = if let Some(name) = project {
        let slug = name.chars().map(|c| if c.is_alphanumeric(){c.to_ascii_lowercase()}else{'-'}).collect::<String>().trim_matches('-').to_string();
        let p = dirs_home().join(".lcl").join("projects").join(format!("{}.sqlite", slug));
        Store::open(&p)?
    } else {
        // find local
        let mut cur = std::env::current_dir().unwrap();
        let mut found = None;
        loop {
            let cand = cur.join(".lcl").join("data.sqlite");
            if cand.exists() { found = Some(cand); break; }
            if !cur.pop() { break; }
        }
        let p = found.ok_or_else(|| anyhow::anyhow!("no project"))?;
        Store::open(&p)?
    };

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut selected: usize = 0;
    let mut status_filter: Option<&str> = None;
    let issues = store.list_issues(None, None, 200).unwrap_or_default();

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(2)]).split(f.area());
            let title = Paragraph::new("lcl TUI  (q quit, j/k nav, Enter view)").block(Block::default().borders(Borders::ALL).title("Linear-lite"));
            f.render_widget(title, chunks[0]);

            let filtered: Vec<&lcl_core::models::Issue> = issues.iter().filter(|i| status_filter.is_none() || i.status.as_str() == status_filter.unwrap()).collect();
            let items: Vec<ListItem> = filtered.iter().map(|i| {
                ListItem::new(format!("#{} [{}] {} - {}", i.number, i.status, i.priority.label(), i.title))
            }).collect();
            let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Issues")).highlight_style(Style::default().bg(Color::DarkGray)).highlight_symbol(">> ");
            let mut state = ListState::default();
            state.select(Some(selected.min(filtered.len().saturating_sub(1))));
            f.render_stateful_widget(list, chunks[1], &mut state);

            let help = Paragraph::new("j/k: move  q: quit  1-6: filter status  0: all").block(Block::default().borders(Borders::ALL));
            f.render_widget(help, chunks[2]);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('j') | KeyCode::Down => { if selected + 1 < issues.len() { selected += 1; } }
                        KeyCode::Char('k') | KeyCode::Up => { if selected > 0 { selected -= 1; } }
                        KeyCode::Char('0') => status_filter = None,
                        KeyCode::Char('1') => status_filter = Some("Backlog"),
                        KeyCode::Char('2') => status_filter = Some("Todo"),
                        KeyCode::Char('3') => status_filter = Some("InProgress"),
                        KeyCode::Char('4') => status_filter = Some("InReview"),
                        KeyCode::Char('5') => status_filter = Some("Done"),
                        KeyCode::Char('6') => status_filter = Some("Canceled"),
                        _ => {}
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}
