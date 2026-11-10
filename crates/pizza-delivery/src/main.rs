mod client;
mod config;
mod handler;
mod server;
mod sqlite_store;

use std::io::IsTerminal;
use std::process::ExitCode as ProcessExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use pizza_delivery_core::{
    render_jobs, render_submitted, render_timeline, ExitCode, OutputMode, Request, Response,
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

    /// Emit JSON lines even on a terminal
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List jobs, or show one job's timeline (id prefix or @last)
    Status {
        job: Option<String>,
        /// Emit JSON lines even on a terminal
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon in the foreground (socket-activated or started on demand)
    Daemon,
    /// Ask the running daemon to exit
    Stop,
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
        Ok(Response::Job { job, events }) => {
            print!("{}", render_timeline(&job, &events, mode));
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
        Some(Command::Daemon) => match server::run(&config) {
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
        Some(Command::Status { job, json }) => request(
            &config,
            Request::Status { job },
            OutputMode::resolve(json || cli.json, tty),
        ),
        None => match cli.prompt {
            Some(prompt) => request(
                &config,
                Request::Submit { prompt },
                OutputMode::resolve(cli.json, tty),
            ),
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
