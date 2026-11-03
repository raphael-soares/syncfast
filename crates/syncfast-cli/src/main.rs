#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::{CommandFactory as _, Parser, Subcommand, ValueEnum};
use jiff::Zoned;
use syncfast_core::bisync::ReviewKind;
use syncfast_core::network::NetworkManager;
use syncfast_core::notify::DesktopNotifier;
use syncfast_core::paths::Paths;
use syncfast_core::preview::{PreviewError, preview};
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, RunError, run};
use syncfast_core::status::{StatusError, collect};
use syncfast_core::units::{ApplyEnv, SystemdMode, UserSystemctl, apply, current_binary};
use syncfast_core::watch::{WatchEnv, WatchError, Watcher, trigger_for};
use tokio::signal::unix::{SignalKind, signal};

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
    /// Reconcile the systemd units with config.toml
    Apply {
        /// Also delete the saved versions of a sync that is no longer in the
        /// config (repeat for several)
        #[arg(long, value_name = "ID")]
        delete_versions: Vec<String>,
    },
    /// Run one round of a sync (what the systemd units call)
    Run {
        /// The sync id from config.toml
        id: String,
    },
    /// Watch the local folders and start a round when they change
    Watch,
    /// Show what a resync or a forced round would change, as JSON
    Preview {
        /// The sync id from config.toml
        id: String,
        #[arg(long, value_enum)]
        kind: Kind,
    },
    /// One line per sync: id, status and last success
    Status,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Kind {
    Resync,
    Force,
}

impl From<Kind> for ReviewKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Resync => Self::Resync,
            Kind::Force => Self::Force,
        }
    }
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

/// The exit code of the first error in the chain that has one; 1 otherwise.
fn failure_code(err: &anyhow::Error) -> u8 {
    if let Some(err) = err.downcast_ref::<RunError>() {
        err.exit_code()
    } else if let Some(err) = err.downcast_ref::<PreviewError>() {
        err.exit_code()
    } else if let Some(err) = err.downcast_ref::<StatusError>() {
        err.exit_code()
    } else if let Some(err) = err.downcast_ref::<WatchError>() {
        err.exit_code()
    } else {
        1
    }
}

fn paths() -> anyhow::Result<Paths> {
    Paths::from_env().context("cannot locate the user directories")
}

async fn dispatch(command: Command) -> anyhow::Result<ExitCode> {
    match command {
        Command::Apply { delete_versions } => apply_units(&delete_versions).await,
        Command::Run { id } => {
            let paths = paths()?;
            let rclone = Rclone::from_env()?;
            let notifier = DesktopNotifier::new();
            let clock = Zoned::now;
            let env = RunEnv {
                paths: &paths,
                rclone: &rclone,
                network: &NetworkManager,
                notifier: &notifier,
                clock: &clock,
                forward_signals: true,
            };
            run(&env, &id).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Watch => {
            watch_folders().await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Preview { id, kind } => {
            let paths = paths()?;
            let rclone = Rclone::from_env()?;
            let notifier = DesktopNotifier::new();
            let clock = Zoned::now;
            let env = RunEnv {
                paths: &paths,
                rclone: &rclone,
                network: &NetworkManager,
                notifier: &notifier,
                clock: &clock,
                forward_signals: true,
            };
            let found = preview(&env, &id, kind.into()).await?;
            println!("{}", serde_json::to_string(&found.report())?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Status => {
            for line in collect(&paths()?).await? {
                println!("{line}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn apply_units(delete_versions: &[String]) -> anyhow::Result<ExitCode> {
    let paths = paths()?;
    let rclone = Rclone::from_env()?;
    let bin = current_binary().context("cannot locate the syncfast binary")?;
    let env = ApplyEnv::new(
        &paths,
        &rclone,
        &bin,
        SystemdMode::from_env(),
        &UserSystemctl,
    );
    let report = apply(&env, delete_versions).await;
    if !report.is_empty() {
        eprint!("{report}");
    }
    Ok(ExitCode::from(report.exit_code()))
}

async fn watch_folders() -> anyhow::Result<()> {
    let paths = paths()?;
    let rclone = Rclone::from_env()?;
    let bin = current_binary().context("cannot locate the syncfast binary")?;
    let trigger = trigger_for(SystemdMode::from_env(), bin, Arc::new(UserSystemctl));
    let watcher = Watcher::start(WatchEnv::new(paths, rclone, trigger)).await?;
    let mut interrupt = signal(SignalKind::interrupt()).context("signal handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("signal handler")?;
    let stop = async {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    };
    watcher.run(stop).await?;
    Ok(())
}
