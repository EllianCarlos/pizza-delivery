mod agent;
mod client;
mod config;
mod git;
mod handler;
mod process;
mod sandbox;
mod server;
mod sqlite_store;
mod triage;
mod worker;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode as ProcessExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use pizza_delivery_core::triage::render_triage;
use pizza_delivery_core::{
    parse_when, render_artifacts, render_events, render_inbox, render_jobs, render_quota,
    render_submitted, render_timeline, ArtifactFilter, EventFilter, ExitCode, InboxFilter,
    OutputMode, Priority, Request, Response,
};

use client::{send, ClientError, Start};
use config::Config;

#[derive(Parser)]
#[command(
    name = "pizza-delivery",
    version,
    about = "Autonomous prompt queue that ends at a draft PR",
    args_conflicts_with_subcommands = true,
    after_help = ExitCode::HELP
)]
struct Cli {
    /// Prompt to enqueue as a new job
    prompt: Option<String>,

    /// Repository the job works in
    #[arg(long)]
    repo: Option<PathBuf>,

    /// Run triage only: print the plan and context sources without enqueuing
    #[arg(long)]
    dry_run: bool,

    /// Emit JSON lines even on a terminal
    #[arg(long)]
    json: bool,

    /// Queue priority of the new job
    #[arg(long, value_enum)]
    priority: Option<PriorityArg>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum PriorityArg {
    Low,
    Normal,
    High,
}

impl From<PriorityArg> for Priority {
    fn from(arg: PriorityArg) -> Self {
        match arg {
            PriorityArg::Low => Priority::Low,
            PriorityArg::Normal => Priority::Normal,
            PriorityArg::High => Priority::High,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// List jobs, or show one job's timeline (id prefix or @last)
    Status {
        job: Option<String>,
        /// Show agent quota state instead of jobs
        #[arg(long, conflicts_with = "job")]
        quota: bool,
        /// Emit JSON lines even on a terminal
        #[arg(long)]
        json: bool,
    },
    /// List feedback entries (assumptions, questions, risks), newest first
    Inbox {
        #[command(flatten)]
        common: ListArgs,
        /// Feedback kind (repeatable)
        #[arg(long)]
        kind: Vec<String>,
        /// Severity (repeatable)
        #[arg(long)]
        severity: Vec<String>,
        /// Minimum confidence, 0-100; entries without confidence are excluded
        #[arg(long)]
        min_confidence: Option<u8>,
    },
    /// List ledger events, newest first
    Events {
        #[command(flatten)]
        common: ListArgs,
        /// Event kind such as stage-failed (repeatable)
        #[arg(long)]
        kind: Vec<String>,
        /// Actor: daemon, agent or human (repeatable)
        #[arg(long)]
        actor: Vec<String>,
    },
    /// List recorded artifacts, newest first
    Artifacts {
        #[command(flatten)]
        common: ListArgs,
        /// Artifact kind (repeatable)
        #[arg(long)]
        kind: Vec<String>,
        /// Path substring (repeatable)
        #[arg(long)]
        path: Vec<String>,
    },
    /// Cancel a job, terminate its running stage and keep its worktree
    Cancel { job: String },
    /// Run the daemon in the foreground (socket-activated or started on demand)
    Daemon,
    /// Ask the running daemon to exit
    Stop,
}

#[derive(clap::Args)]
struct ListArgs {
    /// Job id prefix or @last (repeatable)
    #[arg(long)]
    job: Vec<String>,
    /// Stage id (repeatable)
    #[arg(long)]
    stage: Vec<String>,
    /// Only items at or after this time: RFC 3339 or relative (2h, 3d)
    #[arg(long)]
    since: Option<String>,
    /// Only items at or before this time: RFC 3339 or relative (2h, 3d)
    #[arg(long)]
    until: Option<String>,
    /// Case-insensitive substring search
    #[arg(short = 'q', long = "search")]
    search: Option<String>,
    /// Maximum number of items
    #[arg(long)]
    limit: Option<usize>,
    /// Emit JSON even on a terminal
    #[arg(long)]
    json: bool,
}

type Window = (Option<i64>, Option<i64>);

fn window(args: &ListArgs) -> Result<Window, String> {
    let now = handler::Env::now_ms(&handler::SystemEnv);
    let parse = |v: &Option<String>| v.as_deref().map(|t| parse_when(t, now)).transpose();
    Ok((parse(&args.since)?, parse(&args.until)?))
}

fn list_request(
    common: &ListArgs,
    build: impl FnOnce(Window) -> Request,
) -> Result<Request, ProcessExitCode> {
    match window(common) {
        Ok(w) => Ok(build(w)),
        Err(e) => {
            eprintln!("error: {e}");
            Err(exit(ExitCode::Usage))
        }
    }
}

fn render_triaged(
    outcome: &pizza_delivery_core::triage::Outcome,
    sources: &[pizza_delivery_core::context::SourceUse],
) -> String {
    let value = serde_json::json!({"outcome": outcome, "context_sources": sources});
    format!("{}\n", serde_json::to_string(&value).unwrap_or_default())
}

fn exit(code: ExitCode) -> ProcessExitCode {
    ProcessExitCode::from(code.code())
}

fn request(config: &Config, req: Request, mode: OutputMode) -> ProcessExitCode {
    let reply = send(config, &req, Start::IfNeeded);
    present(reply, mode)
}

fn present(reply: Result<Response, ClientError>, mode: OutputMode) -> ProcessExitCode {
    match reply {
        Ok(Response::Submitted { id, short_id }) => {
            print!("{}", render_submitted(&id, &short_id, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Jobs { jobs }) => {
            print!("{}", render_jobs(&jobs, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Job {
            job,
            stages,
            events,
        }) => {
            print!("{}", render_timeline(&job, &stages, &events, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Quota { agents, now_ms }) => {
            print!("{}", render_quota(&agents, now_ms, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Triaged {
            outcome,
            context_sources,
        }) => {
            if mode == OutputMode::Json {
                print!("{}", render_triaged(&outcome, &context_sources));
            } else {
                print!("{}", render_triage(&outcome, false));
            }
            exit(if outcome.triage.is_some() {
                ExitCode::Success
            } else {
                ExitCode::NotFound
            })
        }
        Ok(Response::Cancelled { id, short_id }) => {
            print!("{}", render_submitted(&id, &short_id, mode));
            exit(ExitCode::Success)
        }
        Ok(r @ Response::InboxItems { .. }) => {
            print!("{}", render_inbox(&r, mode));
            exit(ExitCode::Success)
        }
        Ok(r @ Response::EventItems { .. }) => {
            print!("{}", render_events(&r, mode));
            exit(ExitCode::Success)
        }
        Ok(r @ Response::ArtifactItems { .. }) => {
            print!("{}", render_artifacts(&r, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Stopped) => exit(ExitCode::Success),
        Ok(Response::Error { error }) => {
            eprintln!("error: {}", error.message);
            for id in &error.matches {
                eprintln!("  {id}");
            }
            exit(error.exit_code())
        }
        Err(e @ ClientError::Unreachable(_)) => {
            eprintln!("error: {e}");
            exit(ExitCode::DaemonUnreachable)
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit(ExitCode::General)
        }
    }
}

fn run(cli: Cli) -> ProcessExitCode {
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return exit(ExitCode::General);
        }
    };
    let tty = std::io::stdout().is_terminal();
    match cli.command {
        Some(Command::Daemon) => match config
            .load_routing()
            .map_err(anyhow::Error::from)
            .and_then(|c| server::run(&c))
        {
            Ok(()) => exit(ExitCode::Success),
            Err(e) => {
                eprintln!("error: {e:#}");
                exit(ExitCode::General)
            }
        },
        Some(Command::Stop) => present(
            send(&config, &Request::Stop, Start::Never),
            OutputMode::resolve(false, tty),
        ),
        Some(Command::Inbox {
            common,
            kind,
            severity,
            min_confidence,
        }) => match list_request(&common, |(since_ms, until_ms)| Request::Inbox {
            filter: InboxFilter {
                jobs: common.job.clone(),
                stages: common.stage.clone(),
                kinds: kind,
                severities: severity,
                min_confidence,
                since_ms,
                until_ms,
            },
            query: common.search.clone(),
            limit: common.limit,
        }) {
            Ok(req) => request(
                &config,
                req,
                OutputMode::resolve(common.json || cli.json, tty),
            ),
            Err(code) => code,
        },
        Some(Command::Events {
            common,
            kind,
            actor,
        }) => match list_request(&common, |(since_ms, until_ms)| Request::Events {
            filter: EventFilter {
                jobs: common.job.clone(),
                stages: common.stage.clone(),
                kinds: kind,
                actors: actor,
                since_ms,
                until_ms,
            },
            query: common.search.clone(),
            limit: common.limit,
        }) {
            Ok(req) => request(
                &config,
                req,
                OutputMode::resolve(common.json || cli.json, tty),
            ),
            Err(code) => code,
        },
        Some(Command::Artifacts { common, kind, path }) => {
            match list_request(&common, |(since_ms, until_ms)| Request::Artifacts {
                filter: ArtifactFilter {
                    jobs: common.job.clone(),
                    stages: common.stage.clone(),
                    kinds: kind,
                    paths: path,
                    since_ms,
                    until_ms,
                },
                query: common.search.clone(),
                limit: common.limit,
            }) {
                Ok(req) => request(
                    &config,
                    req,
                    OutputMode::resolve(common.json || cli.json, tty),
                ),
                Err(code) => code,
            }
        }
        Some(Command::Cancel { job }) => request(
            &config,
            Request::Cancel { job },
            OutputMode::resolve(cli.json, tty),
        ),
        Some(Command::Status {
            quota: true, json, ..
        }) => request(
            &config,
            Request::Quota,
            OutputMode::resolve(json || cli.json, tty),
        ),
        Some(Command::Status { job, json, .. }) => request(
            &config,
            Request::Status { job },
            OutputMode::resolve(json || cli.json, tty),
        ),
        None => match cli.prompt {
            Some(prompt) => {
                let repo = cli.repo.map(|r| {
                    std::fs::canonicalize(&r)
                        .unwrap_or(r)
                        .to_string_lossy()
                        .into_owned()
                });
                let req = if cli.dry_run {
                    Request::Triage { prompt, repo }
                } else {
                    Request::Submit {
                        prompt,
                        repo,
                        priority: cli.priority.map(Into::into),
                    }
                };
                request(&config, req, OutputMode::resolve(cli.json, tty))
            }
            None => {
                let _ = Cli::command().print_help();
                exit(ExitCode::Usage)
            }
        },
    }
}

fn main() -> ProcessExitCode {
    run(Cli::parse())
}
