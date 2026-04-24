use crate::models::device::DeviceConfig;
use crate::models::config::AppConfig;
use crate::storage::{load_config, save_config};
use crate::handler::{trigger_backup, ask_user_action};
use crate::models::device::{DeviceAction, BackupRule};

use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io;
use std::time::{Duration, Instant};

pub struct TuiApp {
    pub logs: Vec<String>,
    pub config: AppConfig,
}

impl TuiApp {
    pub fn new() -> Self {
        Self {
            logs: vec!["Starting USBackup TUI...".to_string()],
            config: load_config(),
        }
    }

    pub fn add_log(&mut self, message: String) {
        let now = chrono::Local::now().format("%H:%M:%S");
        self.logs.push(format!("[{}] {}", now, message));
        if self.logs.len() > 20 {
            self.logs.remove(0);
        }
    }
}

pub fn run_tui() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new();
    let tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
                .split(f.area());

            let header = Paragraph::new(" USBackup Agent - Press 'q' to exit ")
                .block(Block::default().borders(Borders::ALL).title(" Info "));
            f.render_widget(header, chunks[0]);

            let log_items: Vec<ListItem> = app.logs.iter()
                .map(|l| ListItem::new(l.as_str()))
                .collect();
            let logs_list = List::new(log_items)
                .block(Block::default().borders(Borders::ALL).title(" Activity Log "));
            f.render_widget(logs_list, chunks[1]);
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') = key.code {
                    break;
                }
            }
        }
        
        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
