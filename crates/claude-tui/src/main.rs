mod app;
mod cli;
mod client;
pub mod format;
mod mock;
mod overview;
mod status_line;
mod theme;
mod widgets;
mod windows;

use app::App;
use claude_common::StatusResponse;
use cli::Command;
use client::DaemonClient;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use std::{io, path::Path, time::Duration};

/// Connect and read timeout for the `status` subcommands. A sketchybar refresh
/// must not wait on a dead daemon (AC-10).
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = match cli::command_from_env() {
        Ok(command) => command,
        Err(usage) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };

    match command {
        Command::StatusLine => {
            let socket = claude_common::socket_path();
            println!("{}", status_line::line(&socket, chrono::Utc::now()).await);
            Ok(())
        }
        Command::Tui { demo } => run_tui(demo).await,
    }
}

/// Ask the daemon for one status response, or `None` when it cannot be reached.
async fn fetch_status(socket: &Path) -> Option<StatusResponse> {
    let client = DaemonClient::connect_with_timeout(socket, STATUS_TIMEOUT).await?;
    client.status().await.ok()
}

async fn run_tui(demo: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Install panic hook to restore terminal on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(panic_info);
    }));

    // The client always exists so the app can retry a dead daemon with
    // `reconnect` on every refresh tick.
    let socket = claude_common::socket_path();
    let client = DaemonClient::disconnected(&socket);
    if !demo {
        client.reconnect().await;
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the app
    let mut app = App::new(client, demo).await;
    let result = app.run(&mut terminal).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    Ok(())
}
