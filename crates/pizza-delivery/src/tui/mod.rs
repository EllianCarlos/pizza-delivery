mod net;
mod ui;

use std::process::{Command as Process, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use pizza_delivery_core::tui::{resume_plan, Caps, Command, Connection, Key, Model, Resume};
use pizza_delivery_core::view::EventView;
use pizza_delivery_core::{Request, Response};
use ratatui::crossterm::event::{self, Event as Input, KeyCode, KeyEventKind, KeyModifiers};

use crate::client::{send, ClientError, Start};
use crate::config::Config;

enum Msg {
    Events(Vec<EventView>),
    Reset,
    Connection(Connection),
    Notice(String),
}

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const PROBE_LIMIT: usize = 1;

pub fn run(config: &Config) -> Result<(), ClientError> {
    send(config, &Request::Status { job: None }, Start::IfNeeded)?;
    let (tx, rx) = channel();
    let port = config.http_port;
    let feeder = tx.clone();
    std::thread::spawn(move || feed(port, &feeder));
    let caps = Caps::from_env();
    let mut terminal = ratatui::init();
    let outcome = event_loop(&mut terminal, config, caps, &tx, &rx);
    ratatui::restore();
    outcome.map_err(|e| ClientError::Protocol(e.to_string()))
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    config: &Config,
    caps: Caps,
    tx: &Sender<Msg>,
    rx: &Receiver<Msg>,
) -> std::io::Result<()> {
    let mut model = Model::new();
    loop {
        terminal.draw(|frame| ui::draw(frame, &model, caps))?;
        if event::poll(Duration::from_millis(100))? {
            if let Input::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if let Some(key) = translate(key.code, key.modifiers) {
                        match model.handle_key(key) {
                            Some(Command::Quit) => return Ok(()),
                            Some(command) => act(config, command, tx),
                            None => {}
                        }
                    }
                }
            }
        }
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Events(events) => model.apply(&events),
                Msg::Reset => model.reset(),
                Msg::Connection(c) => model.set_connection(c),
                Msg::Notice(text) => model.set_notice(text),
            }
        }
    }
}

fn translate(code: KeyCode, modifiers: KeyModifiers) -> Option<Key> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return matches!(code, KeyCode::Char('c')).then_some(Key::Interrupt);
    }
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab => Key::Tab,
        _ => return None,
    })
}

fn feed(port: u16, tx: &Sender<Msg>) {
    let mut last: Option<(u64, String)> = None;
    loop {
        if tx.send(Msg::Connection(Connection::Connecting)).is_err() {
            return;
        }
        let from = match &last {
            None => Some(0),
            Some((cursor, id)) => {
                match net::get_events(port, cursor.saturating_sub(1), PROBE_LIMIT) {
                    Ok(probe) => match resume_plan(Some((*cursor, id)), &probe.events) {
                        Resume::From(c) => Some(c),
                        Resume::Snapshot => {
                            last = None;
                            if tx.send(Msg::Reset).is_err() {
                                return;
                            }
                            Some(0)
                        }
                    },
                    Err(_) => None,
                }
            }
        };
        if let Some(from) = from {
            if let Ok(mut stream) = net::open_stream(port, from) {
                if tx.send(Msg::Connection(Connection::Live)).is_err() {
                    return;
                }
                while let Ok(event) = stream.next_event() {
                    last = Some((event.cursor, event.id.clone()));
                    if tx.send(Msg::Events(vec![event])).is_err() {
                        return;
                    }
                }
            }
        }
        if tx.send(Msg::Connection(Connection::Offline)).is_err() {
            return;
        }
        std::thread::sleep(RECONNECT_DELAY);
    }
}

fn act(config: &Config, command: Command, tx: &Sender<Msg>) {
    let config = config.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let notice = match command {
            Command::Quit => return,
            Command::OpenWeb => open(&config, &config.web_url),
            Command::OpenPr { url } => open(&config, &url),
            Command::Submit { prompt } => call(
                &config,
                Request::Submit {
                    prompt,
                    repo: None,
                    priority: None,
                    options: None,
                },
            ),
            Command::Reply { targets, text } => call(
                &config,
                Request::Reply {
                    targets,
                    text,
                    priority: None,
                },
            ),
            Command::Dismiss { entries } => call(&config, Request::Dismiss { entries }),
            Command::Cancel { job } => call(&config, Request::Cancel { job }),
        };
        let _ = tx.send(Msg::Notice(notice));
    });
}

fn open(config: &Config, target: &str) -> String {
    match Process::new(&config.notify_opener)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => format!("opened {target}"),
        Err(e) => format!("cannot open {target}: {e}"),
    }
}

fn call(config: &Config, request: Request) -> String {
    match send(config, &request, Start::Never) {
        Ok(Response::Submitted { short_id, .. }) => format!("submitted {short_id}"),
        Ok(Response::Replied { short_id, .. }) => format!("follow-up job {short_id} created"),
        Ok(Response::Dismissed { entries }) => format!("dismissed {}", entries.len()),
        Ok(Response::Cancelled { short_id, .. }) => format!("cancelled {short_id}"),
        Ok(Response::Error { error }) => format!("error: {}", error.message),
        Ok(_) => "unexpected reply from the daemon".to_owned(),
        Err(e) => format!("error: {e}"),
    }
}
