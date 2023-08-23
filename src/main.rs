use std::{
    io::{self, BufRead},
    sync::{atomic::Ordering, Arc, Mutex},
};

use clap::Parser;
use tracing::{debug, error, instrument, warn};

static TERMINATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REDRAW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[derive(Debug, Parser)]
struct Args {
    #[arg(
        short,
        long,
        help = "remote address",
        required_unless_present("server")
    )]
    address: Option<String>,
    #[arg(short, long, help = "remote port", default_value_t = 8989)]
    port: u16,
    #[arg(short, long, help = "listening address", default_value_t = 8989)]
    listen: u16,
    #[arg(
        short,
        long,
        help = "run as server",
        required_unless_present("address")
    )]
    server: bool,
    #[arg(short, long, help = "sets the logging level", action=clap::ArgAction::Count)]
    verbose: u8,
}

#[instrument(skip(reader))]
fn reciever<T: std::io::Read>(mut reader: std::io::BufReader<T>, dest: Arc<Mutex<Vec<String>>>) {
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
                if let Ok(mut lock) = dest.lock() {
                    lock.push(buf.trim().to_string());
                    REDRAW.store(true, Ordering::Release);
                }
            }
            Err(e) => warn!("Failed to read data: {e}"),
        }
        buf.clear();
    }
}

type LocalTerminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

#[instrument]
fn init_terminal() -> Result<LocalTerminal, std::io::Error> {
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
fn reset_terminal(mut terminal: LocalTerminal) -> Result<(), std::io::Error> {
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
    // create app and run it
    let app = App::default();
    let res = run_app(&mut terminal, app, &args);
    reset_terminal(terminal)?;
    res?;
    Ok(())
}

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{prelude::*, widgets::*};

enum InputMode {
    Normal,
    Editing,
}

/// App holds the state of the application
struct App {
    /// Current value of the input box
    input: String,
    /// Position of cursor in the editor area.
    cursor_position: usize,
    /// Current input mode
    input_mode: InputMode,
    /// History of recorded messages
    messages: Arc<Mutex<Vec<String>>>,
}

impl Default for App {
    fn default() -> App {
        App {
            input: String::new(),
            input_mode: InputMode::Normal,
            messages: Arc::new(Mutex::new(Vec::new())),
            cursor_position: 0,
        }
    }
}

impl App {
    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.cursor_position.saturating_sub(1);
        self.cursor_position = self.clamp_cursor(cursor_moved_left);
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.cursor_position.saturating_add(1);
        self.cursor_position = self.clamp_cursor(cursor_moved_right);
    }

    fn enter_char(&mut self, new_char: char) {
        self.input.insert(self.cursor_position, new_char);

        self.move_cursor_right();
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.cursor_position != 0;
        if is_not_cursor_leftmost {
            // Method "remove" is not used on the saved text for deleting the selected char.
            // Reason: Using remove on String works on bytes instead of the chars.
            // Using remove would require special care because of char boundaries.

            let current_index = self.cursor_position;
            let from_left_to_current_index = current_index - 1;

            // Getting all characters before the selected character.
            let before_char_to_delete = self.input.chars().take(from_left_to_current_index);
            // Getting all characters after selected character.
            let after_char_to_delete = self.input.chars().skip(current_index);

            // Put all characters together except the selected one.
            // By leaving the selected one out, it is forgotten and therefore deleted.
            self.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.len())
    }

    fn reset_cursor(&mut self) {
        self.cursor_position = 0;
    }

    fn submit_message(&mut self, writer: &mut impl std::io::Write) {
        if let Ok(mut lock) = self.messages.lock() {
            lock.push(self.input.clone());
        } else {
            error!("Failed to lock messages, may be poisoned");
        }
        if let Err(e) = writer.write_all(self.input.as_bytes()) {
            error!("Failed to send message {e}");
        }
        self.input.clear();
        self.reset_cursor();
    }
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App, args: &Args) -> io::Result<()> {
    let mut stream = {
        let address = args.address.as_ref().map_or("0.0.0.0", |s| s.as_str());
        warn!("Waiting for client on {address}:{}", args.port);
        if args.server {
            std::net::TcpListener::bind((address, args.port))?
                .accept()?
                .0
        } else {
            std::net::TcpStream::connect((
                args.address
                    .as_ref()
                    .expect("since server is necessary if the address is not given")
                    .as_str(),
                args.port,
            ))?
        }
    };
    let reader = std::io::BufReader::new(stream.try_clone()?);
    let reciever_buffer = Arc::clone(&app.messages);
    std::thread::spawn(move || reciever(reader, reciever_buffer));
    loop {
        if let Ok(true) = REDRAW.compare_exchange(
            true,
            false,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            terminal.draw(|f| ui(f, &app))?;
        }

        if crossterm::event::poll(std::time::Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                REDRAW.store(true, Ordering::Release);
                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('i') => {
                            app.input_mode = InputMode::Editing;
                        }
                        KeyCode::Char('q') => {
                            return Ok(());
                        }
                        _ => {}
                    },
                    InputMode::Editing if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Enter => app.submit_message(&mut stream),
                        KeyCode::Char(to_insert) => {
                            app.enter_char(to_insert);
                        }
                        KeyCode::Backspace => {
                            app.delete_char();
                        }
                        KeyCode::Left => {
                            app.move_cursor_left();
                        }
                        KeyCode::Right => {
                            app.move_cursor_right();
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)].as_ref())
        .split(f.size());

    let input = Paragraph::new(app.input.as_str())
        .style(match app.input_mode {
            InputMode::Normal => Style::default(),
            InputMode::Editing => Style::default().fg(Color::Yellow),
        })
        .block(Block::default().borders(Borders::ALL).title("Input"));
    f.render_widget(input, chunks[1]);
    match app.input_mode {
        InputMode::Normal =>
            // Hide the cursor. `Frame` does this by default, so we don't need to do anything here
            {}

        InputMode::Editing => {
            // Make the cursor visible and ask ratatui to put it at the specified coordinates after
            // rendering
            f.set_cursor(
                // Draw the cursor at the current position in the input field.
                // This position is can be controlled via the left and right arrow key
                chunks[1].x + app.cursor_position as u16 + 1,
                // Move one line down, from the border to the input line
                chunks[1].y + 1,
            )
        }
    }
    let messages: Vec<ListItem> = {
        let lock = app.messages.lock().unwrap();
        lock[lock.len().saturating_sub(chunks[0].height as usize)..lock.len()]
            .iter()
            .map(|m| {
                let content = Line::from(Span::raw(format!("> {m}")));
                ListItem::new(content)
            })
            .collect()
    };
    let messages =
        List::new(messages).block(Block::default().borders(Borders::ALL).title("Messages"));
    f.render_widget(messages, chunks[0]);
}
