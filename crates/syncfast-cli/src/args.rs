//! The command line, as clap reads it.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use syncfast_core::bisync::ReviewKind;
use syncfast_core::ops::{
    DEFAULT_DEPTH, DEFAULT_FILE_LIMIT, DEFAULT_MAX_FOLDERS, DEFAULT_WAIT_SECONDS, Keep,
};
use syncfast_core::recently_deleted::DeletedSide;

#[derive(Debug, Parser)]
#[command(
    name = "syncfast",
    version,
    about = "Keep folders in sync with Google Drive"
)]
pub struct Cli {
    /// Print the result as JSON (errors too) instead of text
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
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
    /// One line per sync: id, status and last success (`--json`: everything
    /// about each sync)
    Status,
    /// Serve the operations to an AI agent over MCP (stdio)
    Mcp,
    /// Print the guide for AI agents (markdown)
    AgentGuide,
    /// Run a planned copy or move (what `copy` and `move` start on their own)
    #[command(hide = true)]
    TransferRun {
        /// The transfer id
        id: String,
    },
    /// Run one round of a backup (what the backup units call)
    #[command(hide = true)]
    BackupRun {
        /// The backup id from config.toml
        id: String,
    },
    /// Run a restore that was asked for (what a restore starts on its own)
    #[command(hide = true)]
    RestoreRun {
        /// The restore id
        id: String,
    },
    #[command(flatten)]
    Operation(Operation),
}

/// What a person or an agent does with their syncs: each is one operation
/// of the core.
#[derive(Debug, Subcommand)]
pub enum Operation {
    /// Show one sync: folders, filters, status, gate and the last round
    Show {
        /// The sync id
        id: String,
    },
    /// Add a sync; it starts held for the review of its first round
    Add(AddArgs),
    /// Remove a sync; files on both sides stay
    Remove {
        /// The sync id
        id: String,
        /// Also delete the saved versions of the sync
        #[arg(long)]
        delete_versions: bool,
    },
    /// Pause a sync, or all of them
    Pause(Target),
    /// Resume a paused sync, or all of them
    Resume(Target),
    /// Presets and custom rules of a sync
    #[command(subcommand)]
    Filters(FiltersCommand),
    /// List the rclone accounts (names and types, never secrets)
    Accounts,
    /// List the folders directly inside a remote folder
    Folders {
        /// `account:folder`, for example `gdrive:` or `gdrive:Notes`
        path: String,
    },
    /// Create a remote folder
    Mkdir {
        /// `account:folder`
        path: String,
    },
    /// Summarize a local or remote folder: counts, sizes, extensions, dates
    /// (metadata only, never file content)
    Explore {
        /// A path on this computer, or `account:folder`
        target: String,
        /// How many levels of folders to list
        #[arg(long, default_value_t = DEFAULT_DEPTH)]
        depth: usize,
        /// The most folders to list
        #[arg(long, default_value_t = DEFAULT_MAX_FOLDERS)]
        max_folders: usize,
    },
    /// Copy a file or folder, on this computer or on a remote, to a new place
    Copy(TransferArgs),
    /// Move a file or folder, on this computer or on a remote, to a new place
    Move(TransferArgs),
    /// Read a copy or move that was still running when its command returned
    Transfer {
        /// The transfer id that `copy` or `move` returned
        id: String,
        #[command(flatten)]
        wait: Wait,
    },
    /// Start a round of a sync now, or of all enabled syncs
    Sync {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        wait: Wait,
    },
    /// The review a held sync waits for
    #[command(subcommand)]
    Review(ReviewCommand),
    /// Conflicts: both versions of a file were kept
    #[command(subcommand)]
    Conflicts(ConflictsCommand),
    /// Files a sync deleted or replaced that can still be restored
    #[command(subcommand)]
    Deleted(DeletedCommand),
    /// The recent rounds of a sync
    Activity {
        /// The sync id
        id: String,
    },
    /// What a round logged
    Log {
        /// The sync id
        id: String,
        /// The round, as `activity` lists it
        round: String,
    },
    /// Show or set the window theme
    Theme {
        /// `system`, `light`, `dark` or a file of the themes folder
        name: Option<String>,
    },
    /// One line per backup: id, status and last success (`--json`:
    /// everything about each backup)
    Backups,
    /// Backups: snapshots, files, runs, checks, settings and restores
    #[command(subcommand)]
    Backup(BackupCommand),
    /// Show or change how syncs behave: for one sync, or the defaults
    Settings {
        #[command(subcommand)]
        command: Option<SettingsCommand>,
        /// The sync to read or change; without it, the defaults every sync inherits
        #[arg(long, global = true, value_name = "ID")]
        sync: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    /// Show one backup: folders, schedule, keep, status and the last run
    /// and check
    Show {
        /// The backup id
        id: String,
    },
    /// List the snapshots of a backup, newest first
    Snapshots {
        /// The backup id
        id: String,
        /// The most snapshots to list (the newest); all when absent
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// List the files of a snapshot folder: names, sizes and dates, never
    /// content
    Files {
        /// The backup id
        id: String,
        /// `latest`, or a snapshot id (or the start of one)
        snapshot: String,
        /// A folder of the snapshot, as `files` printed it; the top when
        /// absent
        dir: Option<String>,
        /// The most entries to list; more are left out and the answer says so
        #[arg(long, value_name = "N", default_value_t = DEFAULT_FILE_LIMIT)]
        max_entries: usize,
    },
    /// Back up now
    Run {
        /// The backup id
        id: String,
        #[command(flatten)]
        wait: Wait,
    },
    /// Check the repository of a backup
    Check {
        /// The backup id
        id: String,
        /// Read a sample of the data too
        #[arg(long, conflicts_with = "full")]
        sample: bool,
        /// Read all the data
        #[arg(long)]
        full: bool,
        #[command(flatten)]
        wait: Wait,
    },
    /// Pause the schedule of a backup, or of all
    Pause(BackupTarget),
    /// Resume the schedule of a backup, or of all
    Resume(BackupTarget),
    /// Create a backup with a new key; the key stays in the key storage and
    /// the user saves it in syncfast
    Add(BackupAddArgs),
    /// Change one setting: `name`, `paths`, `every`, `keep.daily`,
    /// `keep.weekly`, `keep.monthly`, `keep.yearly`, `presets`, `filters`,
    /// `limit_upload`, `limit_download` or `one_file_system`. Settings that
    /// lower safety are for the user to change
    Set {
        /// The backup id
        id: String,
        /// The setting
        key: String,
        /// `true` or `false`, a number, or text; any number of values for
        /// paths, presets and filters (none empties the list)
        #[arg(num_args = 0.., allow_hyphen_values = true)]
        value: Vec<String>,
    },
    /// Removing a backup is for the user: this refuses
    Remove {
        /// The backup id
        id: String,
    },
    /// Restore a snapshot into a new folder
    Restore {
        /// The backup id
        id: String,
        /// `latest`, or a snapshot id (or the start of one)
        snapshot: String,
        /// The folder to create; it must not exist
        #[arg(long)]
        to: String,
        /// A folder or file of the snapshot to restore (repeat); all when
        /// none is given
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
        /// Say what would be restored (files, bytes) and change nothing
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        wait: Wait,
    },
    /// Read a restore that was still running when its command returned
    Job {
        /// The restore id that `backup restore` returned
        id: String,
        #[command(flatten)]
        wait: Wait,
    },
}

#[derive(Debug, clap::Args)]
pub struct BackupTarget {
    /// The backup id
    #[arg(required_unless_present = "all")]
    pub id: Option<String>,
    /// Every backup
    #[arg(long, conflicts_with = "id")]
    pub all: bool,
}

#[derive(Debug, clap::Args)]
pub struct BackupAddArgs {
    /// Name shown for the backup
    #[arg(long)]
    pub name: String,
    /// A folder on this computer to back up (repeat; `~` is the home folder)
    #[arg(long = "path", value_name = "PATH", required = true)]
    pub paths: Vec<String>,
    /// Where the backup goes: a folder on this computer or `account:folder`;
    /// it must be empty or new
    #[arg(long)]
    pub dest: String,
    /// `hourly`, `daily` or `weekly`
    #[arg(long)]
    pub every: Option<String>,
    /// How many to keep, as `daily=7,weekly=4,monthly=6,yearly=0`; what is
    /// not given has the default
    #[arg(long)]
    pub keep: Option<String>,
    /// A preset to turn on (repeat); the defaults when none is given
    #[arg(long = "preset", value_name = "NAME")]
    pub presets: Vec<String>,
    /// Turn every preset off
    #[arg(long, conflicts_with = "presets")]
    pub no_presets: bool,
    /// A custom filter rule, `- pattern` or `+ pattern` (repeat)
    #[arg(long = "filter", value_name = "RULE", allow_hyphen_values = true)]
    pub filters: Vec<String>,
    /// Upload limit, such as `1M`, or `off`
    #[arg(long)]
    pub limit_upload: Option<String>,
    /// Download limit, such as `1M`, or `off`
    #[arg(long)]
    pub limit_download: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Kind {
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

/// One sync, or all of them.
#[derive(Debug, clap::Args)]
pub struct Target {
    /// The sync id
    #[arg(required_unless_present = "all")]
    pub id: Option<String>,
    /// Every sync
    #[arg(long, conflicts_with = "id")]
    pub all: bool,
}

#[derive(Debug, clap::Args)]
pub struct Wait {
    /// Seconds to wait for the round or transfer to end; 0 returns at once.
    /// It goes on by itself after the wait.
    #[arg(long, value_name = "SECONDS", default_value_t = DEFAULT_WAIT_SECONDS)]
    pub wait: u64,
}

#[derive(Debug, clap::Args)]
pub struct TransferArgs {
    /// A file or folder on this computer (`~` is the home folder), or
    /// `account:path`
    pub source: String,
    /// The new file or folder itself, not the folder it goes into. It must
    /// not exist: nothing is overwritten or merged
    pub destination: String,
    /// Say what would be transferred (files, bytes) and change nothing
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub wait: Wait,
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Name shown for the sync
    #[arg(long)]
    pub name: String,
    /// Folder on this computer (`~` is the home folder)
    #[arg(long)]
    pub local: String,
    /// Remote folder, `account:folder`
    #[arg(long)]
    pub remote: String,
    /// Sync id; made from the name when absent
    #[arg(long)]
    pub id: Option<String>,
    /// A preset to turn on (repeat); `system_junk`, `dev_dirs` and `git_repos` when none is given
    #[arg(long = "preset", value_name = "NAME")]
    pub presets: Vec<String>,
    /// Turn every preset off
    #[arg(long, conflicts_with = "presets")]
    pub no_presets: bool,
    /// A custom filter rule, `- pattern` or `+ pattern` (repeat)
    #[arg(long = "rule", value_name = "RULE", allow_hyphen_values = true)]
    pub rules: Vec<String>,
    /// Create the local folder when it does not exist
    #[arg(long)]
    pub create_local: bool,
    /// Create the remote folder when it does not exist
    #[arg(long)]
    pub create_remote: bool,
}

#[derive(Debug, Subcommand)]
pub enum FiltersCommand {
    /// Set the presets and custom rules of a sync; what is not given stays
    Set {
        /// The sync id
        id: String,
        /// A preset to turn on (repeat); replaces the current presets
        #[arg(long = "preset", value_name = "NAME")]
        presets: Vec<String>,
        /// Turn every preset off
        #[arg(long, conflicts_with = "presets")]
        no_presets: bool,
        /// A custom rule, `- pattern` or `+ pattern` (repeat); replaces the
        /// current rules
        #[arg(long = "rule", value_name = "RULE", allow_hyphen_values = true)]
        rules: Vec<String>,
        /// Remove every custom rule
        #[arg(long, conflicts_with = "rules")]
        clear_rules: bool,
    },
    /// Count the files the rules leave out, without changing anything
    Test {
        /// A sync to test (its folder and rules unless overridden)
        id: Option<String>,
        /// A folder on this computer, when there is no sync yet
        #[arg(long, conflicts_with = "id")]
        local: Option<String>,
        /// Presets to test with (repeat); none given: the sync's, or `system_junk`, `dev_dirs` and `git_repos`
        #[arg(long = "preset", value_name = "NAME")]
        presets: Vec<String>,
        /// Test with no preset
        #[arg(long, conflicts_with = "presets")]
        no_presets: bool,
        /// Rules to test with (repeat); none given: the sync's
        #[arg(long = "rule", value_name = "RULE", allow_hyphen_values = true)]
        rules: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    /// Dry-run the review a held sync waits for and show what would change
    Show {
        /// The sync id
        id: String,
    },
    /// Confirm the review that was shown, when it deletes nothing
    Confirm {
        /// The sync id
        id: String,
        /// The `filters_hash` of the review that was shown
        #[arg(long)]
        filters_hash: String,
        #[command(flatten)]
        wait: Wait,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConflictsCommand {
    /// List the conflicts of a sync
    List {
        /// The sync id
        id: String,
    },
    /// Settle a conflict and sync the result
    Resolve {
        /// The sync id
        id: String,
        /// The conflicted copy, as `conflicts list` shows it
        path: PathBuf,
        #[arg(long, value_enum)]
        keep: Choice,
    },
}

#[derive(Debug, Subcommand)]
pub enum DeletedCommand {
    /// List what the sync deleted or replaced that can still come back
    List {
        /// The sync id
        id: String,
    },
    /// Restore a file into the sync's folder, never over another, and sync it
    Restore {
        /// The sync id
        id: String,
        /// The key `deleted list` shows
        key: String,
        #[arg(long, value_enum)]
        side: Side,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Side {
    /// The versions folder of the sync, on this computer
    Local,
    /// The versions a mirror to Drive keeps on the account
    Remote,
    /// The Drive trash
    Drive,
}

impl From<Side> for DeletedSide {
    fn from(side: Side) -> Self {
        match side {
            Side::Local => Self::Local,
            Side::Remote => Self::Remote,
            Side::Drive => Self::Drive,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Set a setting; settings that lower safety are for the user to change
    ///
    /// `app.autostart` only writes the config. The Flatpak window asks the
    /// desktop to start syncfast at login the next time it opens.
    Set {
        /// A setting name, as `settings` lists it
        key: String,
        /// `true` or `false`, a whole number, or text such as `30s` or `newer`
        value: String,
    },
    /// Remove the sync's own value, or the changed default, so it falls back
    Reset {
        /// A setting name, as `settings` lists it
        key: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Choice {
    /// Delete the conflicted copy
    Current,
    /// The conflicted copy becomes the file; the current one goes to the
    /// saved versions
    Conflicted,
    /// Keep the conflicted copy beside the file under a new name
    Both,
}

impl From<Choice> for Keep {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::Current => Self::Current,
            Choice::Conflicted => Self::Conflicted,
            Choice::Both => Self::Both,
        }
    }
}
