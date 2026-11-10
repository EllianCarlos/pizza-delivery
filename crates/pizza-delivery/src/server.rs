use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use listenfd::ListenFd;
use pizza_delivery_core::api::{ApiError, ErrorKind};
use pizza_delivery_core::{Request, Response, Store};

use crate::agent::{CliAgent, Fleet};
use crate::config::Config;
use crate::git::SystemGit;
use crate::handler::{handle, Env, SystemEnv};
use crate::sandbox::{ensure_compatible, load_trust, SystemProbe};
use crate::sqlite_store::SqliteStore;
use crate::triage::{
    analyze, AgentTieBreaker, ContextSource, ConventionsSource, Deps, EngramSource, LedgerSource,
    MnemonSource, SystemIndex, Triager,
};
use crate::worker::{Worker, WorkerConfig};
use pizza_delivery_core::routing::AdapterKind;

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
    let trust = load_trust(&config.trust_path)?;
    ensure_compatible(
        &trust,
        &config.agent_command.to_string_lossy(),
        &SystemProbe,
    )?;
    let store: Box<dyn Store> = Box::new(SqliteStore::open(&config.db_path())?);
    serve(listener, store, Arc::new(SystemEnv), config)
}

fn with_deps<T>(
    store: &Mutex<Box<dyn Store>>,
    config: &Config,
    agent: &CliAgent,
    f: impl FnOnce(Deps<'_>) -> T,
) -> T {
    let index = SystemIndex {
        projects_dir: config.projects_dir.clone(),
        engram_db: config.engram_db.clone(),
    };
    let tie_breaker = AgentTieBreaker {
        agent,
        cwd: config.data_dir.clone(),
    };
    let engram = EngramSource {
        command: config.engram_command.clone(),
    };
    let mnemon = MnemonSource {
        command: config.mnemon_command.clone(),
    };
    let ledger = LedgerSource { store };
    let sources: Vec<&dyn ContextSource> = vec![&engram, &mnemon, &ledger, &ConventionsSource];
    f(Deps {
        index: &index,
        tie_breaker: &tie_breaker,
        sources,
    })
}

fn dry_run(
    store: &Mutex<Box<dyn Store>>,
    config: &Config,
    prompt: &str,
    repo: Option<&str>,
) -> Response {
    let agent = CliAgent::new(AdapterKind::Claude, &config.agent_command);
    let report = with_deps(store, config, &agent, |deps| analyze(prompt, repo, &deps));
    Response::Triaged {
        outcome: report.outcome,
        context_sources: report.context.map(|c| c.included).unwrap_or_default(),
    }
}

fn work_loop(store: &Mutex<Box<dyn Store>>, env: &dyn Env, config: &Config, stop: &AtomicBool) {
    let fleet = Fleet::from_routing(&config.routing);
    let agent = CliAgent::new(AdapterKind::Claude, &config.agent_command);
    let trust = match load_trust(&config.trust_path) {
        Ok(trust) => trust,
        Err(e) => {
            eprintln!("trust config: {e:#}");
            return;
        }
    };
    let worker_config = WorkerConfig {
        trust,
        data_dir: config.data_dir.clone(),
        holder: format!("daemon-{}", std::process::id()),
        lease_ttl_ms: config.lease_ttl_ms,
        heartbeat_ms: u64::try_from(config.lease_ttl_ms / 3)
            .unwrap_or(1000)
            .max(10),
        timeout: Duration::from_secs(config.stage_timeout_secs),
        budget_cap_tokens: config.budget_cap_tokens,
        require_triage: true,
    };
    let worker = Worker {
        store,
        env,
        git: &SystemGit,
        fleet: &fleet,
        routing: &config.routing,
        config: &worker_config,
    };
    while !stop.load(Ordering::SeqCst) {
        let triaged = with_deps(store, config, &agent, |deps| {
            Triager {
                store,
                env,
                data_dir: &config.data_dir,
                deps,
                agent: agent.name(),
                budget_cap_tokens: config.budget_cap_tokens,
            }
            .tick()
        });
        match triaged {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => eprintln!("triage: {e:#}"),
        }
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
                let config = config.clone();
                std::thread::spawn(move || {
                    let _ = connection(stream, &store, env.as_ref(), &stop, &config);
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
    config: &Config,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(Request::Triage { prompt, .. }) if prompt.trim().is_empty() => {
            failure(ErrorKind::Usage, "prompt must not be empty")
        }
        Ok(Request::Triage { prompt, repo }) => dry_run(store, config, &prompt, repo.as_deref()),
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
