use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use listenfd::ListenFd;
use pizza_delivery_core::api::{ApiError, ErrorKind};
use pizza_delivery_core::{Request, Response, Store};

use crate::agent::ClaudeAgent;
use crate::config::Config;
use crate::git::SystemGit;
use crate::handler::{handle, Env, SystemEnv};
use crate::sqlite_store::SqliteStore;
use crate::worker::{Worker, WorkerConfig};

pub enum Bound {
    Listener(UnixListener),
    AlreadyRunning,
}

pub fn bind(config: &Config) -> Result<Bound> {
    let mut fds = ListenFd::from_env();
    if let Some(listener) = fds.take_unix_listener(0).context("inherited socket")? {
        return Ok(Bound::Listener(listener));
    }
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent).context("create socket directory")?;
    }
    if config.socket_path.exists() {
        if UnixStream::connect(&config.socket_path).is_ok() {
            return Ok(Bound::AlreadyRunning);
        }
        std::fs::remove_file(&config.socket_path).context("remove stale socket")?;
    }
    match UnixListener::bind(&config.socket_path) {
        Ok(listener) => Ok(Bound::Listener(listener)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Ok(Bound::AlreadyRunning),
        Err(e) => Err(e).context("bind socket"),
    }
}

pub fn run(config: &Config) -> Result<()> {
    let listener = match bind(config)? {
        Bound::Listener(l) => l,
        Bound::AlreadyRunning => return Ok(()),
    };
    std::fs::create_dir_all(&config.data_dir).context("create data directory")?;
    let store: Box<dyn Store> = Box::new(SqliteStore::open(&config.db_path())?);
    serve(listener, store, Arc::new(SystemEnv), config)
}

fn work_loop(store: &Mutex<Box<dyn Store>>, env: &dyn Env, config: &Config, stop: &AtomicBool) {
    let agent = ClaudeAgent::new(&config.agent_command);
    let worker_config = WorkerConfig {
        data_dir: config.data_dir.clone(),
        holder: format!("daemon-{}", std::process::id()),
        lease_ttl_ms: config.lease_ttl_ms,
        heartbeat_ms: u64::try_from(config.lease_ttl_ms / 3)
            .unwrap_or(1000)
            .max(10),
        timeout: Duration::from_secs(config.stage_timeout_secs),
    };
    let worker = Worker {
        store,
        env,
        git: &SystemGit,
        agent: &agent,
        config: &worker_config,
    };
    while !stop.load(Ordering::SeqCst) {
        match worker.tick() {
            Ok(true) => {}
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                eprintln!("worker: {e:#}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

pub fn serve(
    listener: UnixListener,
    store: Box<dyn Store>,
    env: Arc<dyn Env>,
    config: &Config,
) -> Result<()> {
    listener
        .set_nonblocking(true)
        .context("nonblocking listener")?;
    let store = Arc::new(Mutex::new(store));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_config = config.clone();
    let worker_handle = {
        let store = Arc::clone(&store);
        let env = Arc::clone(&env);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || work_loop(&store, env.as_ref(), &worker_config, &stop))
    };
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let store = Arc::clone(&store);
                let env = Arc::clone(&env);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let _ = connection(stream, &store, env.as_ref(), &stop);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).context("accept"),
        }
    }
    drop(worker_handle);
    Ok(())
}

fn connection(
    stream: UnixStream,
    store: &Mutex<Box<dyn Store>>,
    env: &dyn Env,
    stop: &AtomicBool,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => {
            let stopping = request == Request::Stop;
            let response = match store.lock() {
                Ok(mut guard) => handle(request, guard.as_mut(), env),
                Err(_) => failure(ErrorKind::Internal, "store lock poisoned"),
            };
            if stopping {
                stop.store(true, Ordering::SeqCst);
            }
            response
        }
        Err(e) => failure(ErrorKind::Usage, format!("bad request: {e}")),
    };
    let mut out = &stream;
    writeln!(out, "{}", serde_json::to_string(&response)?)?;
    Ok(())
}

fn failure(kind: ErrorKind, message: impl Into<String>) -> Response {
    Response::Error {
        error: ApiError {
            kind,
            message: message.into(),
            matches: vec![],
        },
    }
}
