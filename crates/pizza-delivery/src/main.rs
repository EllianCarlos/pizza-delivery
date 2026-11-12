mod agent;
mod client;
mod config;
mod git;
mod handler;
mod http;
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
use pizza_delivery_core::triage::{render_triage, Template, TriageOptions};
use pizza_delivery_core::{
    render_jobs, render_quota, render_submitted, render_timeline, ExitCode, OutputMode, Priority,
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
    /// Run the daemon in the foreground (socket-activated or started on demand)
    Daemon,
    /// Ask the running daemon to exit
    Stop,
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
        Some(Command::Stop) => present(
            send(&config, &Request::Stop, Start::Never),
            OutputMode::resolve(false, tty),
        ),
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
