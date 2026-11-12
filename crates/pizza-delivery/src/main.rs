mod access;
mod agent;
mod assets;
mod client;
mod config;
mod git;
mod handler;
mod http;
mod lock;
mod mcp_broker;
mod notify;
mod process;
mod push;
mod remote;
mod routing_check;
mod sandbox;
mod server;
mod sqlite_store;
mod triage;
#[cfg(feature = "tui")]
mod tui;
mod worker;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode as ProcessExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use pizza_delivery_core::triage::{render_triage, Template, TriageOptions};
use pizza_delivery_core::{
    parse_when, render_artifacts, render_dismissed, render_events, render_inbox,
    render_inbox_entry, render_jobs, render_quota, render_replied, render_submitted,
    render_timeline, ArtifactFilter, EventFilter, ExitCode, InboxFilter, OutputMode, Priority,
    Request, Response,
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

    /// Plan template, overriding the one triage would pick from the prompt
    #[arg(
        long,
        value_enum,
        value_name = "TEMPLATE",
        help_heading = "Additional Parameters"
    )]
    template: Option<TemplateArg>,

    /// Stop with findings: drop the implement, review and ship stages
    #[arg(long, help_heading = "Additional Parameters")]
    report_only: bool,

    /// Cap the number of planned stages (1 to 10)
    #[arg(long, value_name = "N", help_heading = "Additional Parameters")]
    max_stages: Option<usize>,

    /// Extra context for triage, appended to the restated task
    #[arg(
        long = "context",
        value_name = "TEXT",
        help_heading = "Additional Parameters"
    )]
    context_hint: Option<String>,

    /// Extra risk flag to assert (kebab-case, repeatable)
    #[arg(
        long = "risk",
        value_name = "FLAG",
        help_heading = "Additional Parameters"
    )]
    risk_flags: Vec<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum TemplateArg {
    FullFeature,
    BugFix,
    Investigate,
}

impl From<TemplateArg> for Template {
    fn from(arg: TemplateArg) -> Self {
        match arg {
            TemplateArg::FullFeature => Template::FullFeature,
            TemplateArg::BugFix => Template::BugFix,
            TemplateArg::Investigate => Template::Investigate,
        }
    }
}

impl Cli {
    /// Options are sent only when at least one Additional Parameter is set.
    fn options(&self) -> Option<TriageOptions> {
        let options = TriageOptions {
            template: self.template.map(Into::into),
            report_only: self.report_only,
            max_stages: self.max_stages,
            context_hint: self.context_hint.clone(),
            risk_flags: self.risk_flags.clone(),
        };
        (!options.is_empty()).then_some(options)
    }
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
enum RoutingAction {
    /// Validate the routing file and print the resolved agent/model chain per stage
    Check,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect agent routing without the daemon
    Routing {
        #[command(subcommand)]
        action: RoutingAction,
    },
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
    /// List feedback entries (assumptions, questions, risks), show one, or dismiss entries
    Inbox {
        #[command(flatten)]
        common: ListArgs,
        /// Feedback kind (repeatable)
        #[arg(long)]
        kind: Vec<String>,
        /// Severity (repeatable)
        #[arg(long)]
        severity: Vec<String>,
        /// Entry status: open, answered or dismissed (repeatable); overrides --all
        #[arg(long)]
        status: Vec<String>,
        /// Minimum confidence, 0-100; entries without confidence are excluded
        #[arg(long)]
        min_confidence: Option<u8>,
        /// Include dismissed entries
        #[arg(long)]
        all: bool,
        #[command(subcommand)]
        action: Option<InboxAction>,
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
    /// Write systemd user units (socket and service) for non-Nix Linux
    InstallService,
    /// Print shell completions to stdout
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Open the terminal UI
    #[cfg(feature = "tui")]
    Tui,
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

#[cfg(feature = "tui")]
fn run_tui(config: &Config) -> ProcessExitCode {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        eprintln!("error: the tui needs an interactive terminal");
        return exit(ExitCode::Usage);
    }
    match tui::run(config) {
        Ok(()) => exit(ExitCode::Success),
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
        Ok(r @ Response::EventItems { .. }) => {
            print!("{}", render_events(&r, mode));
            exit(ExitCode::Success)
        }
        Ok(r @ Response::ArtifactItems { .. }) => {
            print!("{}", render_artifacts(&r, mode));
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

const SOCKET_UNIT: &str = "[Unit]
Description=pizza-delivery daemon socket

[Socket]
ListenStream=%t/pizza-delivery.sock

[Install]
WantedBy=sockets.target
";

fn service_unit(exe: &std::path::Path) -> String {
    format!(
        "[Unit]
Description=pizza-delivery daemon
Requires=pizza-delivery.socket
After=pizza-delivery.socket

[Service]
ExecStart={} daemon
Restart=on-failure
",
        exe.display()
    )
}

fn install_service() -> ProcessExitCode {
    let result = (|| -> anyhow::Result<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or_else(|| anyhow::anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))?;
        let dir = base.join("systemd/user");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("pizza-delivery.socket"), SOCKET_UNIT)?;
        std::fs::write(
            dir.join("pizza-delivery.service"),
            service_unit(&std::env::current_exe()?),
        )?;
        Ok(dir)
    })();
    match result {
        Ok(dir) => {
            println!("wrote units to {}", dir.display());
            println!("systemctl --user daemon-reload");
            println!("systemctl --user enable --now pizza-delivery.socket");
            exit(ExitCode::Success)
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            exit(ExitCode::General)
        }
    }
}

fn run(cli: Cli) -> ProcessExitCode {
    match cli.command {
        Some(Command::InstallService) => return install_service(),
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "pizza-delivery", &mut std::io::stdout());
            return exit(ExitCode::Success);
        }
        _ => {}
    }
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
    let options = cli.options();
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
        Some(Command::McpBroker { .. } | Command::InstallService | Command::Completions { .. }) => {
            exit(ExitCode::General)
        }
        Some(Command::Routing {
            action: RoutingAction::Check,
        }) => match config.load_routing() {
            Ok(c) => {
                let (text, ok) = routing_check::run(&c);
                print!("{text}");
                exit(if ok {
                    ExitCode::Success
                } else {
                    ExitCode::General
                })
            }
            Err(e) => {
                eprintln!("error: {e}");
                exit(ExitCode::General)
            }
        },
        #[cfg(feature = "tui")]
        Some(Command::Tui) => run_tui(&config),
        Some(Command::Stop) => present(
            send(&config, &Request::Stop, Start::Never),
            OutputMode::resolve(false, tty),
        ),
        Some(Command::Inbox {
            common,
            kind,
            severity,
            status,
            min_confidence,
            all,
            action,
        }) => match action {
            Some(action) => {
                let req = match action {
                    InboxAction::Show { entry } => Request::InboxShow { entry },
                    InboxAction::Dismiss { entries } => Request::Dismiss { entries },
                };
                request(
                    &config,
                    req,
                    OutputMode::resolve(common.json || cli.json, tty),
                )
            }
            None => match list_request(&common, |(since_ms, until_ms)| Request::Inbox {
                all,
                filter: InboxFilter {
                    jobs: common.job.clone(),
                    stages: common.stage.clone(),
                    kinds: kind,
                    severities: severity,
                    statuses: status,
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
                    Request::Triage {
                        prompt,
                        repo,
                        options,
                    }
                } else {
                    Request::Submit {
                        prompt,
                        repo,
                        priority: cli.priority.map(Into::into),
                        options,
                    }
                };
                request(&config, req, OutputMode::resolve(cli.json, tty))
            }
            None => {
                #[cfg(feature = "tui")]
                if tty && std::io::stdin().is_terminal() {
                    return run_tui(&config);
                }
                let _ = Cli::command().print_help();
                exit(ExitCode::Usage)
            }
        },
    }
}

fn main() -> ProcessExitCode {
    run(Cli::parse())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("pizza-delivery").chain(args.iter().copied()))
    }

    #[test]
    fn no_additional_parameters_sends_no_options() {
        assert_eq!(parse(&["do it"]).options(), None);
    }

    #[test]
    fn additional_parameters_map_onto_options() {
        let cli = parse(&[
            "do it",
            "--template",
            "bug-fix",
            "--report-only",
            "--max-stages",
            "3",
            "--context",
            "see docs",
            "--risk",
            "production",
            "--risk",
            "customer-data",
        ]);
        assert_eq!(
            cli.options(),
            Some(TriageOptions {
                template: Some(Template::BugFix),
                report_only: true,
                max_stages: Some(3),
                context_hint: Some("see docs".into()),
                risk_flags: vec!["production".into(), "customer-data".into()],
            })
        );
    }

    #[test]
    fn help_groups_the_flags_under_additional_parameters() {
        let help = Cli::command().render_long_help().to_string();
        let (_, group) = help
            .split_once("Additional Parameters:")
            .unwrap_or_default();
        for flag in [
            "--template",
            "--report-only",
            "--max-stages",
            "--context",
            "--risk",
        ] {
            assert!(group.contains(flag), "{flag} missing");
        }
    }
}
