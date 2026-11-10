mod agent;
mod assets;
mod client;
mod config;
mod git;
mod handler;
mod http;
mod mcp_broker;
mod notify;
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
    render_dismissed, render_inbox, render_inbox_entry, render_jobs, render_quota, render_replied,
    render_submitted, render_timeline, ExitCode, OutputMode, Priority, Request, Response,
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
    /// Cancel a job, terminate its running stage and keep its worktree
    Cancel { job: String },
    /// List feedback entries, show one, or dismiss entries
    Inbox {
        /// Include dismissed entries
        #[arg(long)]
        all: bool,
        /// Emit JSON lines even on a terminal
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<InboxAction>,
    },
    /// Answer feedback entries of one job with a new follow-up job
    Reply {
        /// A job (all its open entries) or entry ids
        #[arg(required = true)]
        targets: Vec<String>,
        /// Reply text; opens $EDITOR when omitted
        #[arg(short, long)]
        message: Option<String>,
        /// Queue priority of the follow-up job (default high)
        #[arg(long, value_enum)]
        priority: Option<PriorityArg>,
        /// Emit JSON lines even on a terminal
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon in the foreground (socket-activated or started on demand)
    Daemon,
    /// Ask the running daemon to exit
    Stop,
    /// Serve the per-stage MCP broker on stdio (started by the agent session)
    #[command(hide = true)]
    McpBroker {
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        stage: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        secrets: Option<PathBuf>,
    },
}

fn run_broker(
    catalog: PathBuf,
    stage: String,
    agent: String,
    log: PathBuf,
    secrets: Option<PathBuf>,
) -> ProcessExitCode {
    let catalog = std::fs::read_to_string(&catalog)
        .map_err(anyhow::Error::from)
        .and_then(|text| pizza_delivery_core::mcp::Catalog::parse(&text).map_err(Into::into));
    let result = catalog.and_then(|catalog| {
        mcp_broker::serve(
            mcp_broker::Settings {
                catalog,
                stage,
                agent,
                log,
                secrets,
            },
            std::io::BufReader::new(std::io::stdin()),
            std::io::stdout(),
        )
    });
    match result {
        Ok(()) => exit(ExitCode::Success),
        Err(e) => {
            eprintln!("error: {e:#}");
            exit(ExitCode::General)
        }
    }
}

#[derive(Subcommand)]
enum InboxAction {
    /// Show one entry in full
    Show { entry: String },
    /// Dismiss entries without creating a job
    Dismiss {
        #[arg(required = true)]
        entries: Vec<String>,
    },
}

fn edit_reply() -> Result<String, String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());
    let path = std::env::temp_dir().join(format!("pizza-delivery-reply-{}.md", std::process::id()));
    std::fs::write(&path, "").map_err(|e| e.to_string())?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .map_err(|e| e.to_string());
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => text,
        Ok(s) => Err(format!("editor exited with {s}")),
        Err(e) => Err(e),
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
        Ok(Response::Inbox { entries }) => {
            print!("{}", render_inbox(&entries, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::InboxEntry { entry }) => {
            print!("{}", render_inbox_entry(&entry, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Dismissed { entries }) => {
            print!("{}", render_dismissed(&entries, mode));
            exit(ExitCode::Success)
        }
        Ok(Response::Replied {
            id,
            short_id,
            answered,
        }) => {
            print!("{}", render_replied(&id, &short_id, &answered, mode));
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
    if let Some(Command::McpBroker {
        catalog,
        stage,
        agent,
        log,
        secrets,
    }) = cli.command
    {
        return run_broker(catalog, stage, agent, log, secrets);
    }
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
        Some(Command::McpBroker { .. }) => exit(ExitCode::General),
        Some(Command::Stop) => present(
            send(&config, &Request::Stop, Start::Never),
            OutputMode::resolve(false, tty),
        ),
        Some(Command::Cancel { job }) => request(
            &config,
            Request::Cancel { job },
            OutputMode::resolve(cli.json, tty),
        ),
        Some(Command::Inbox { all, json, action }) => {
            let req = match action {
                None => Request::Inbox { all },
                Some(InboxAction::Show { entry }) => Request::InboxShow { entry },
                Some(InboxAction::Dismiss { entries }) => Request::Dismiss { entries },
            };
            request(&config, req, OutputMode::resolve(json || cli.json, tty))
        }
        Some(Command::Reply {
            targets,
            message,
            priority,
            json,
        }) => {
            let text = match message {
                Some(text) => text,
                None => match edit_reply() {
                    Ok(text) => text,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return exit(ExitCode::General);
                    }
                },
            };
            request(
                &config,
                Request::Reply {
                    targets,
                    text,
                    priority: priority.map(Into::into),
                },
                OutputMode::resolve(json || cli.json, tty),
            )
        }
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
