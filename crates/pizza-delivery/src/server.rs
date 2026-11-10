use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use listenfd::ListenFd;
use pizza_delivery_core::api::{ApiError, ErrorKind};
use pizza_delivery_core::{ExpiredLease, JobId, Request, Response, Store};

use crate::agent::{CliAgent, Fleet};
use crate::config::Config;
use crate::git::{Git, LockedGit, SystemGit};
use crate::handler::{handle, Env, SystemEnv};
use crate::http;
use crate::process::stop_leased_group;
use crate::sandbox::{ensure_compatible, load_trust, SystemProbe};
use crate::sqlite_store::SqliteStore;
use crate::triage::{
    analyze, AgentTieBreaker, ContextSource, ConventionsSource, Deps, EngramSource, LedgerSource,
    MnemonSource, SystemIndex, Triager,
};
use crate::worker::{McpSetup, Worker, WorkerConfig};
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
        grace: Duration::from_millis(config.kill_grace_ms),
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

fn worker_config(config: &Config) -> Result<WorkerConfig> {
    Ok(WorkerConfig {
        trust: load_trust(&config.trust_path)?,
        data_dir: config.data_dir.clone(),
        holder: format!("daemon-{}", std::process::id()),
        lease_ttl_ms: config.lease_ttl_ms,
        heartbeat_ms: u64::try_from(config.lease_ttl_ms / 3)
            .unwrap_or(1000)
            .max(10),
        timeout: config.stage_timeout_secs.map(Duration::from_secs),
        kill_grace: Duration::from_millis(config.kill_grace_ms),
        caps: config.caps,
        retention_ms: config.retention_ms,
        budget_cap_tokens: config.budget_cap_tokens,
        budget_defaults: config.budget_defaults,
        require_triage: true,
        mcp: config.mcp_catalog.exists().then(|| McpSetup {
            broker: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pizza-delivery")),
            catalog: config.mcp_catalog.clone(),
            secrets: config.mcp_secrets.clone(),
        }),
    })
}

fn work_loop(
    store: &Mutex<Box<dyn Store>>,
    env: &dyn Env,
    git: &dyn Git,
    config: &Config,
    stop: &AtomicBool,
) {
    let fleet = Fleet::from_routing(&config.routing);
    let worker_config = match worker_config(config) {
        Ok(worker_config) => worker_config,
        Err(e) => {
            eprintln!("trust config: {e:#}");
            return;
        }
    };
    let worker = Worker {
        store,
        env,
        git,
        fleet: &fleet,
        routing: &config.routing,
        config: &worker_config,
    };
    while !stop.load(Ordering::SeqCst) {
        match worker.run_once() {
            Ok(true) => {}
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                eprintln!("worker: {e:#}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

fn triage_loop(store: &Mutex<Box<dyn Store>>, env: &dyn Env, config: &Config, stop: &AtomicBool) {
    let agent = CliAgent::new(AdapterKind::Claude, &config.agent_command);
    while !stop.load(Ordering::SeqCst) {
        let triaged = with_deps(store, config, &agent, |deps| {
            Triager {
                store,
                env,
                data_dir: &config.data_dir,
                deps,
                agent: agent.name(),
                budget_cap_tokens: config.budget_cap_tokens,
                budget_defaults: config.budget_defaults,
            }
            .tick()
        });
        match triaged {
            Ok(true) => continue,
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                eprintln!("triage: {e:#}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

const SWEEP_EVERY: Duration = Duration::from_secs(60);

fn janitor_loop(
    store: &Mutex<Box<dyn Store>>,
    env: &dyn Env,
    git: &dyn Git,
    config: &Config,
    stop: &AtomicBool,
) {
    let fleet = Fleet::from_routing(&config.routing);
    let worker_config = match worker_config(config) {
        Ok(worker_config) => worker_config,
        Err(e) => {
            eprintln!("trust config: {e:#}");
            return;
        }
    };
    let worker = Worker {
        store,
        env,
        git,
        fleet: &fleet,
        routing: &config.routing,
        config: &worker_config,
    };
    let mut last_sweep = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if let Err(e) = worker.reap() {
            eprintln!("janitor: {e:#}");
        }
        if last_sweep.elapsed() >= SWEEP_EVERY {
            last_sweep = Instant::now();
            if let Err(e) = worker.sweep() {
                eprintln!("janitor: {e:#}");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn join_within(handles: Vec<JoinHandle<()>>, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && handles.iter().any(|h| !h.is_finished()) {
        std::thread::sleep(Duration::from_millis(20));
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
    let git = Arc::new(LockedGit::new(SystemGit));
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    {
        let fleet = Fleet::from_routing(&config.routing);
        let worker_config = worker_config(config)?;
        let recovery = Worker {
            store: store.as_ref(),
            env: env.as_ref(),
            git: git.as_ref(),
            fleet: &fleet,
            routing: &config.routing,
            config: &worker_config,
        };
        if let Err(e) = recovery.reap() {
            eprintln!("recovery: {e:#}");
        }
    }
    for _ in 0..config.caps.global.max(1) {
        let (store, env, git, stop) = (
            Arc::clone(&store),
            Arc::clone(&env),
            Arc::clone(&git),
            Arc::clone(&stop),
        );
        let config = config.clone();
        handles.push(std::thread::spawn(move || {
            work_loop(&store, env.as_ref(), git.as_ref(), &config, &stop)
        }));
    }
    {
        let (store, env, stop) = (Arc::clone(&store), Arc::clone(&env), Arc::clone(&stop));
        let config = config.clone();
        handles.push(std::thread::spawn(move || {
            triage_loop(&store, env.as_ref(), &config, &stop)
        }));
    }
    {
        let (store, env, git, stop) = (
            Arc::clone(&store),
            Arc::clone(&env),
            Arc::clone(&git),
            Arc::clone(&stop),
        );
        let config = config.clone();
        std::thread::spawn(move || {
            janitor_loop(&store, env.as_ref(), git.as_ref(), &config, &stop)
        });
    }
    let kill_grace = Duration::from_millis(config.kill_grace_ms);
    match TcpListener::bind(("127.0.0.1", config.http_port)) {
        Ok(http_listener) => {
            let api = http::Api {
                store: Arc::clone(&store),
                env: Arc::clone(&env),
                stop: Arc::clone(&stop),
                kill_grace,
            };
            std::thread::spawn(move || http::listen(http_listener, api));
        }
        Err(e) => eprintln!("http api disabled: port {}: {e}", config.http_port),
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let store = Arc::clone(&store);
                let env = Arc::clone(&env);
                let stop = Arc::clone(&stop);
                let config = config.clone();
                std::thread::spawn(move || {
                    let _ = connection(stream, &store, env.as_ref(), &stop, &config, kill_grace);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).context("accept"),
        }
    }
    join_within(handles, Duration::from_millis(config.shutdown_grace_ms));
    Ok(())
}

fn stop_running_stage(store: &Mutex<Box<dyn Store>>, job: &JobId, grace: Duration) {
    let lease = store
        .lock()
        .ok()
        .and_then(|guard| guard.lease_of_job(job).ok().flatten());
    if let Some(ExpiredLease {
        pid: Some(pid),
        pid_start: Some(start),
        ..
    }) = lease
    {
        stop_leased_group(pid, start, grace);
    }
}

fn connection(
    stream: UnixStream,
    store: &Mutex<Box<dyn Store>>,
    env: &dyn Env,
    stop: &AtomicBool,
    config: &Config,
    kill_grace: Duration,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(Request::Triage { prompt, .. }) if prompt.trim().is_empty() => {
            failure(ErrorKind::Usage, "prompt must not be empty")
        }
        Ok(Request::Triage { prompt, repo }) => dry_run(store, config, &prompt, repo.as_deref()),
        Ok(request) => dispatch(request, store, env, stop, kill_grace),
        Err(e) => failure(ErrorKind::Usage, format!("bad request: {e}")),
    };
    let mut out = &stream;
    writeln!(out, "{}", serde_json::to_string(&response)?)?;
    Ok(())
}

pub fn dispatch(
    request: Request,
    store: &Mutex<Box<dyn Store>>,
    env: &dyn Env,
    stop: &AtomicBool,
    kill_grace: Duration,
) -> Response {
    let stopping = request == Request::Stop;
    let response = match store.lock() {
        Ok(mut guard) => handle(request, guard.as_mut(), env),
        Err(_) => failure(ErrorKind::Internal, "store lock poisoned"),
    };
    if stopping {
        stop.store(true, Ordering::SeqCst);
    }
    if let Response::Cancelled { id, .. } = &response {
        stop_running_stage(store, id, kill_grace);
    }
    response
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
