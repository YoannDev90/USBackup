use crate::models::config::AppConfig;
use crate::storage::load_config;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use std::io;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub enum TuiEvent {
    DeviceConnected(String),
    BackupStarted(String),
    BackupSuccess(String),
    Log(String),
}

pub struct TuiApp {
    pub logs: Vec<String>,
    pub config: AppConfig,
}

impl TuiApp {
    pub fn new() -> Self {
        Self {
            logs: vec!["Service de surveillance démarré...".to_string()],
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

pub fn run_tui(rx: Receiver<TuiEvent>) -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new();
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        // Traitement des messages asynchrones
        while let Ok(event) = rx.try_recv() {
            match event {
                TuiEvent::DeviceConnected(name) => app.add_log(format!("[+] Appareil : {}", name)),
                TuiEvent::BackupStarted(name) => app.add_log(format!("[...] Backup : {}", name)),
                TuiEvent::BackupSuccess(name) => app.add_log(format!("[OK] Succès : {}", name)),
                TuiEvent::Log(msg) => app.add_log(msg),
            }
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Length(3),
                        Constraint::Min(0),
                        Constraint::Length(3),
                    ]
                    .as_ref(),
                )
                .split(f.area());

            let header = Paragraph::new(" USBackup Agent 24h/24 ")
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .block(Block::default().borders(Borders::ALL).title(" Statut "));
            f.render_widget(header, chunks[0]);

            let log_items: Vec<ListItem> = app
                .logs
                .iter()
                .map(|l| {
                    let style = if l.contains("[OK]") {
                        Style::default().fg(Color::Green)
                    } else if l.contains("[...]") {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(l.as_str()).style(style)
                })
                .collect();

            let logs_list = List::new(log_items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Journal d'activité "),
            );
            f.render_widget(logs_list, chunks[1]);

            let footer =
                Paragraph::new(" 'q' pour quitter | Surveillance active | Mode: Production ")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(Block::default().borders(Borders::TOP));
            f.render_widget(footer, chunks[2]);
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
