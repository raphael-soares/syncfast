#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use anyhow::Context as _;
use clap::{CommandFactory as _, Parser, Subcommand};
use jiff::Zoned;
use syncfast_core::network::NetworkManager;
use syncfast_core::notify::DesktopNotifier;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, RunError, run};

#[derive(Debug, Parser)]
#[command(
    name = "syncfast",
    version,
    about = "Keep folders in sync with Google Drive"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one round of a sync (what the systemd units call)
    Run {
        /// The sync id from config.toml
        id: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Err(err) = fastframe_log::Logging::new("syncfast", env!("CARGO_PKG_VERSION")).init() {
        eprintln!("syncfast: cannot start logging: {err}");
    }
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return match Cli::command().print_help() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    };
    match dispatch(command).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("syncfast: {err:#}");
            ExitCode::from(failure_code(&err))
        }
    }
}

fn failure_code(err: &anyhow::Error) -> u8 {
    err.downcast_ref::<RunError>()
        .map_or(1, RunError::exit_code)
}

async fn dispatch(command: Command) -> anyhow::Result<ExitCode> {
    match command {
        Command::Run { id } => {
            let paths = Paths::from_env().context("cannot locate the user directories")?;
            let rclone = Rclone::from_env()?;
            let notifier = DesktopNotifier::new();
            let clock = Zoned::now;
            let env = RunEnv {
                paths: &paths,
                rclone: &rclone,
                network: &NetworkManager,
                notifier: &notifier,
                clock: &clock,
            };
            run(&env, &id).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
