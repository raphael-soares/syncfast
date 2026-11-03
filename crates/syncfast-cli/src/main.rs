#![allow(clippy::print_stdout, clippy::print_stderr)]

mod args;
mod mcp;
mod render;

use args::{Cli, Command, ConflictsCommand, FiltersCommand, Operation, ReviewCommand};

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::{CommandFactory as _, Parser as _};
use jiff::Zoned;
use serde::Serialize;
use syncfast_core::flatpak;
use syncfast_core::network::NetworkManager;
use syncfast_core::notify::DesktopNotifier;
use syncfast_core::ops::{ExploreOptions, NewSync, OpError, Ops, RoundOutcome};
use syncfast_core::paths::Paths;
use syncfast_core::preview::{PreviewError, preview};
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, RunError, run};
use syncfast_core::status::{StatusError, collect};
use syncfast_core::units::{ApplyEnv, SystemdMode, UserSystemctl, apply, current_binary};
use syncfast_core::watch::{WatchEnv, WatchError, Watcher, trigger_for};
use tokio::signal::unix::{SignalKind, signal};

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
    match dispatch(command, cli.json).await {
        Ok(code) => code,
        Err(err) => report_failure(&err, cli.json),
    }
}

/// Prints the error as the user asked for it (JSON on stdout with
/// `--json`, text on stderr otherwise) and gives the exit code.
fn report_failure(err: &anyhow::Error, json: bool) -> ExitCode {
    if json {
        let code = err
            .downcast_ref::<OpError>()
            .map_or("failed", OpError::code);
        let body = serde_json::json!({"error": {"code": code, "message": format!("{err:#}")}});
        println!("{body}");
    } else {
        eprintln!("syncfast: {err:#}");
    }
    ExitCode::from(failure_code(err))
}

/// The exit code of the first error in the chain that has one; 1 otherwise.
fn failure_code(err: &anyhow::Error) -> u8 {
    if let Some(err) = err.downcast_ref::<OpError>() {
        err.exit_code()
    } else if let Some(err) = err.downcast_ref::<RunError>() {
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

/// Prints `value` as one line of JSON, or `text(value)` as it is.
fn emit<T: Serialize>(
    json: bool,
    value: &T,
    text: impl FnOnce(&T) -> String,
) -> anyhow::Result<ExitCode> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        print!("{}", text(value));
    }
    Ok(ExitCode::SUCCESS)
}

/// The preset names given with `--preset`, or `None` to leave them as they
/// are; `--no-presets` is the empty list.
fn preset_choice(presets: Vec<String>, none: bool) -> Option<Vec<String>> {
    if none {
        Some(Vec::new())
    } else if presets.is_empty() {
        None
    } else {
        Some(presets)
    }
}

async fn dispatch(command: Command, json: bool) -> anyhow::Result<ExitCode> {
    match command {
        Command::Apply { delete_versions } => {
            if json {
                let view = Ops::from_env()?.apply(&delete_versions).await;
                println!("{}", serde_json::to_string(&view)?);
                return Ok(ExitCode::from(view.exit_code));
            }
            apply_units(&delete_versions).await
        }
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
            let report = run(&env, &id).await?;
            if json {
                println!("{}", serde_json::to_string(&RoundOutcome::from(report))?);
            }
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
            if json {
                let syncs = Ops::from_env()?.status().await?;
                return emit(true, &syncs, |_| String::new());
            }
            for line in collect(&paths()?).await? {
                println!("{line}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Mcp => {
            mcp::serve(Ops::from_env()?).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::AgentGuide => {
            let guide = include_str!("../../../docs/agents.md");
            if json {
                println!("{}", serde_json::json!({ "markdown": guide }));
            } else {
                print!("{guide}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Operation(operation) => operate(operation, json).await,
    }
}

/// The commands that are an operation of the core and nothing else.
async fn operate(operation: Operation, json: bool) -> anyhow::Result<ExitCode> {
    let ops = Ops::from_env()?;
    match operation {
        Operation::Show { id } => emit(json, &ops.show(&id).await?, render::show),
        Operation::Add(add) => {
            let new = NewSync {
                name: add.name,
                local: add.local,
                remote: add.remote,
                id: add.id,
                presets: preset_choice(add.presets, add.no_presets),
                filters: add.rules,
                create_local: add.create_local,
                create_remote: add.create_remote,
            };
            emit(json, &ops.add(new).await?, render::added)
        }
        Operation::Remove {
            id,
            delete_versions,
        } => emit(
            json,
            &ops.remove(&id, delete_versions).await?,
            render::removed,
        ),
        Operation::Pause(target) => emit(
            json,
            &ops.set_enabled(target.id.as_deref(), false).await?,
            render::toggled,
        ),
        Operation::Resume(target) => emit(
            json,
            &ops.set_enabled(target.id.as_deref(), true).await?,
            render::toggled,
        ),
        Operation::Filters(command) => filters(&ops, command, json).await,
        Operation::Review(command) => review(&ops, command, json).await,
        Operation::Conflicts(command) => conflicts(&ops, command, json).await,
        Operation::Accounts => emit(json, &ops.accounts().await?, |found| {
            render::accounts(found)
        }),
        Operation::Folders { path } => emit(json, &ops.folders(&path).await?, render::folders),
        Operation::Mkdir { path } => {
            emit(json, &ops.make_folder(&path).await?, render::folder_made)
        }
        Operation::Explore {
            target,
            depth,
            max_folders,
        } => {
            let options = ExploreOptions { depth, max_folders };
            emit(json, &ops.explore(&target, options).await?, render::explore)
        }
        Operation::Sync { target, wait } => {
            let rounds = ops
                .sync_now(target.id.as_deref(), Duration::from_secs(wait.wait))
                .await?;
            emit(json, &rounds, render::sync_rounds)
        }
        Operation::Activity { id } => emit(json, &ops.activity(&id).await?, |found| {
            render::activity(found)
        }),
        Operation::Log { id, round } => {
            emit(json, &ops.round_log(&id, &round).await?, render::round_log)
        }
        Operation::Theme { name: None } => emit(json, &ops.theme().await, render::theme_info),
        Operation::Theme { name: Some(name) } => {
            emit(json, &ops.set_theme(&name).await?, render::theme_set)
        }
    }
}

async fn filters(ops: &Ops, command: FiltersCommand, json: bool) -> anyhow::Result<ExitCode> {
    match command {
        FiltersCommand::Set {
            id,
            presets,
            no_presets,
            rules,
            clear_rules,
        } => {
            let rules = if clear_rules {
                Some(Vec::new())
            } else if rules.is_empty() {
                None
            } else {
                Some(rules)
            };
            let saved = ops
                .set_filters(&id, preset_choice(presets, no_presets), rules)
                .await?;
            emit(json, &saved, render::filters_saved)
        }
        FiltersCommand::Test {
            id,
            local,
            presets,
            no_presets,
            rules,
        } => {
            let rules = (!rules.is_empty()).then_some(rules);
            let found = ops
                .test_filters(
                    id.as_deref(),
                    local.as_deref(),
                    preset_choice(presets, no_presets),
                    rules,
                )
                .await?;
            emit(json, &found, render::filter_test)
        }
    }
}

async fn review(ops: &Ops, command: ReviewCommand, json: bool) -> anyhow::Result<ExitCode> {
    match command {
        ReviewCommand::Show { id } => emit(json, &ops.show_review(&id).await?, render::review),
        ReviewCommand::Confirm {
            id,
            filters_hash,
            wait,
        } => {
            let confirmed = ops
                .confirm_review(&id, &filters_hash, Duration::from_secs(wait.wait))
                .await?;
            emit(json, &confirmed, render::confirmed)
        }
    }
}

async fn conflicts(ops: &Ops, command: ConflictsCommand, json: bool) -> anyhow::Result<ExitCode> {
    match command {
        ConflictsCommand::List { id } => emit(json, &ops.conflicts(&id).await?, |found| {
            render::conflicts(found)
        }),
        ConflictsCommand::Resolve { id, path, keep } => {
            let resolved = ops.resolve_conflict(&id, &path, keep.into()).await?;
            emit(json, &resolved, render::resolved)
        }
    }
}

async fn apply_units(delete_versions: &[String]) -> anyhow::Result<ExitCode> {
    let paths = paths()?;
    let rclone = Rclone::from_env()?;
    let bin = current_binary().context("cannot locate the syncfast binary")?;
    let flatpak_id = flatpak::app_id();
    let systemctl = UserSystemctl::detect();
    let env = ApplyEnv::new(&paths, &rclone, &bin, SystemdMode::from_env(), &systemctl)
        .with_flatpak_id(flatpak_id.as_deref());
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
    let trigger = trigger_for(
        SystemdMode::from_env(),
        bin,
        Arc::new(UserSystemctl::detect()),
    );
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
