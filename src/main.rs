use std::{io::BufRead, sync::atomic::Ordering};

use clap::Parser;
use tracing::{debug, error, info, instrument, warn};

static TERMINATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, Parser)]
struct Args {
    #[arg(short, long, help = "remote address")]
    address: String,
    #[arg(short, long, help = "remote port", default_value_t = 8989)]
    port: u16,
    #[arg(short, long, help = "listening address", default_value_t = 8989)]
    listen: u16,
    #[arg(short, long, help = "run as server")]
    server: bool,
    #[arg(short, long, help = "sets the logging level", action=clap::ArgAction::Count)]
    verbose: u8,
}

#[instrument(skip(reader))]
fn reciever<T: std::io::Read>(mut reader: std::io::BufReader<T>) {
    let mut buf = String::new();
    'read: while !TERMINATE.load(std::sync::atomic::Ordering::Acquire) {
        match reader.read_line(&mut buf) {
            Ok(size) => {
                if size == 0 {
                    warn!("May be other end is closed!");
                    TERMINATE.store(true, Ordering::Release);
                    break 'read;
                };
                debug!("recieved data: {:?}", buf.as_bytes());
                println!("{}", buf.trim());
            }
            Err(e) => warn!("Failed to read data: {e}"),
        }
        buf.clear();
    }
}

#[instrument(skip(writer))]
fn write_msgs(mut writer: impl std::io::Write) {
    let mut buf = String::new();
    let mut stdin = std::io::BufReader::new(std::io::stdin());
    'write: while !TERMINATE.load(std::sync::atomic::Ordering::Acquire) {
        match stdin.read_line(&mut buf) {
            Ok(size) => {
                if size == 0 {
                    info!("Closing connection");
                    TERMINATE.store(true, Ordering::Release);
                    break 'write;
                }
                if let Err(e) = writer.write_all(buf.as_bytes()) {
                    warn!("Failed to write to remote: {e}");
                } else {
                    debug!("sent: {:?}", buf.as_bytes());
                }
                buf.clear();
            }
            Err(e) => {
                error!("Failed to read user input: {e}");
            }
        }
    }
}

type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

#[instrument]
fn init_terminal() -> Result<Terminal, std::io::Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    ratatui::Terminal::new(backend)
}
#[instrument(skip(terminal))]
fn reset_terminal(mut terminal: Terminal) -> Result<(), std::io::Error> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
#[instrument]
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let level = match args.verbose {
        0 => tracing::Level::WARN,
        1 => tracing::Level::INFO,
        2 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
    debug!("setting log level to {level}");
    let mut terminal = init_terminal()?;
    terminal.draw(|f| {
        let size = f.size();
        let block = ratatui::widgets::Block::default()
            .title("chatterbox")
            .borders(ratatui::widgets::Borders::ALL);
        f.render_widget(block, size);
    })?;
    let stream = std::net::TcpStream::connect((args.address, args.port))?;
    let reader = std::io::BufReader::new(stream.try_clone()?);
    std::thread::spawn(move || reciever(reader));
    write_msgs(stream);
    reset_terminal(terminal)?;
    Ok(())
}
