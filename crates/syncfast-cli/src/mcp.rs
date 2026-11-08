//! `syncfast mcp`: the operations of `syncfast_core::ops` as MCP tools over
//! stdio. A tool only translates its arguments and the result; the logic
//! is the core's (ADR 0007).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt as _, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use syncfast_core::backup::state::CheckKind;
use syncfast_core::ops::{
    BackupEdit, BackupOptions, DEFAULT_DEPTH, DEFAULT_FILE_LIMIT, DEFAULT_MAX_FOLDERS,
    DEFAULT_WAIT_SECONDS, ExploreOptions, Keep, KeepPartial, NewBackupRequest, NewSync, OpError,
    Ops, RestoreArgs, TransferOptions,
};

const INSTRUCTIONS: &str = "\
syncfast keeps folders on this computer in sync with a cloud account (rclone bisync). \
You can do what the user can do in syncfast, except signing in: creating an account or \
changing its credentials is for the user.

Recommended flow:
1. Explore. list_accounts, then list_remote_folders and explore on the remote \
(`account:folder`) and on the local folder. Explore reads names, sizes and dates only, \
never the content of a file. Use it to decide what to sync and what to leave out.
2. Propose. Tell the user which local folder pairs with which remote folder and which \
filters you suggest. test_filters says how many files a rule leaves out before anything is saved.
3. Add. add_sync with the folders the user agreed to (create_local and create_remote only \
when they agreed to create folders). The new sync is held for a review: nothing is copied yet.
4. Review. show_review lists what the first sync would change. When agent_can_confirm is \
true (nothing is deleted), call confirm_review with the filters_hash it returned. When the \
review deletes files you cannot confirm it: ask the user to review it in syncfast.
5. Follow. status and show_sync say where each sync stands; sync_now runs a round; \
list_conflicts and resolve_conflict settle files that changed on both sides; list_activity \
and show_round_log explain what a round did.

6. Organize. copy and move take a file or folder on this computer or `account:path` on either \
side. The destination is the new name itself and must not exist. Call with dry_run first to show \
the user the files and bytes. A transfer that outlasts the wait goes on by itself: read it with \
show_transfer. A folder that a sync keeps cannot be moved: remove the sync first.

Rules: a changed filter or an error holds the sync for a new review. A sync held because many \
files would be deleted is for the user to confirm. A first sync of a large folder can take \
longer than the wait: the round goes on by itself, read status later. get_settings shows how a \
sync behaves and where each value comes from; set_setting and reset_setting change it, except \
what lowers safety (a larger max_delete, versions off, conflict_loser delete, a switch to a mirror mode, \
review moving towards automatic): that is for the user.

Backups (restic, encrypted, to a folder or a cloud account). list_backups and show_backup \
say how each stands; list_snapshots and list_snapshot_files read what a backup holds \
(names, sizes and dates only, never content); backup_now and check_backup run now and wait \
like sync_now. To protect a folder, create_backup with the folders and a destination the user \
agreed to; it makes the key itself and keeps it in the key storage. You never see or enter \
a key. A new backup stays \"Save the key\" until the user opens syncfast and confirms they \
saved it: tell them to. edit_backup changes name, folders, every, keep, presets, filters and \
limits, but not what lowers safety: keeping fewer snapshots, removing a folder, turning \
one_file_system off and changing the destination are refused with exit 4 for the user to do. \
remove_backup always refuses: only the user removes a backup. restore_snapshot restores into \
a folder that does not exist yet, never over files: call it with dry_run first, then tell the \
user where the files went. A restore that outlasts the wait goes on by itself: read it with \
show_backup_job.";

/// The MCP server: one tool per operation.
#[derive(Clone)]
pub struct SyncfastMcp {
    ops: Arc<Ops>,
}

fn reply<T: Serialize>(result: Result<T, OpError>) -> CallToolResult {
    let outcome = result.map_err(|err| err.to_string()).and_then(|value| {
        serde_json::to_string(&value).map_err(|err| format!("cannot encode the result: {err}"))
    });
    match outcome {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
    }
}

fn wait(seconds: Option<u64>) -> Duration {
    Duration::from_secs(seconds.unwrap_or(DEFAULT_WAIT_SECONDS))
}

/// One sync, or all of them: exactly one of the two must be asked for.
fn one_or_all(
    id: Option<String>,
    all: Option<bool>,
    noun: &str,
) -> Result<Option<String>, OpError> {
    match (id, all.unwrap_or(false)) {
        (Some(id), false) => Ok(Some(id)),
        (None, true) => Ok(None),
        _ => Err(OpError::Invalid(format!(
            "give either id (one {noun}) or all = true (every {noun})"
        ))),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IdArgs {
    /// The sync id, as `status` shows it.
    id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AddSyncArgs {
    /// Name shown for the sync.
    name: String,
    /// Folder on this computer; `~` is the home folder.
    local: String,
    /// Remote folder as `account:folder`, for example `gdrive:Notes`; `gdrive:` is the whole account.
    remote: String,
    /// Sync id (a-z, 0-9, `-`, up to 32 characters); made from the name when absent.
    id: Option<String>,
    /// Presets to turn on: `system_junk`, `dev_dirs`, `build_artifacts` and `git_repos`. `system_junk`, `dev_dirs` and `git_repos` when absent; `[]` turns them off.
    presets: Option<Vec<String>>,
    /// Custom filter rules, each `- pattern` (exclude) or `+ pattern` (include).
    filters: Option<Vec<String>>,
    /// Create the local folder when it does not exist. Only when the user agreed.
    create_local: Option<bool>,
    /// Create the remote folder when it does not exist. Only when the user agreed.
    create_remote: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RemoveSyncArgs {
    /// The sync id.
    id: String,
    /// Also delete the saved versions of replaced and deleted local files.
    delete_versions: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OneOrAllArgs {
    /// The sync id.
    id: Option<String>,
    /// Every sync, instead of one. Give `id` or `all`, not both.
    all: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetFiltersArgs {
    /// The sync id.
    id: String,
    /// The presets to turn on (`system_junk`, `dev_dirs`, `build_artifacts`, `git_repos`); replaces the current ones. Unchanged when absent.
    presets: Option<Vec<String>>,
    /// Custom rules, each `- pattern` or `+ pattern`; replaces the current ones. Unchanged when absent.
    rules: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TestFiltersArgs {
    /// A sync to test: its folder and rules, unless overridden below.
    id: Option<String>,
    /// A folder on this computer, to test before a sync exists. Give `id` or `local`.
    local: Option<String>,
    /// Presets to test with. Absent: the sync's, or `system_junk`, `dev_dirs` and `git_repos`.
    presets: Option<Vec<String>>,
    /// Rules to test with. Absent: the sync's, or none.
    rules: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PathArgs {
    /// A remote folder as `account:folder`; `gdrive:` is the whole account.
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExploreArgs {
    /// A folder on this computer (absolute, or starting with `~`), or a remote folder as `account:folder`.
    target: String,
    /// How many levels of folders to list below the target. Default 2.
    depth: Option<usize>,
    /// The most folders to list, biggest first, so the answer stays short. Default 60.
    max_folders: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SyncNowArgs {
    /// The sync id.
    id: Option<String>,
    /// Every enabled sync, instead of one. Give `id` or `all`, not both.
    all: Option<bool>,
    /// Seconds to wait for the round to end (default 60; 0 returns at once). The round goes on by itself.
    wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TransferArgs {
    /// A file or folder on this computer (absolute, or `~/...`) or `account:path`.
    source: String,
    /// The new file or folder itself, not the folder it goes into. It must not exist: nothing is overwritten or merged.
    destination: String,
    /// Only report the files and bytes that would be transferred and change nothing.
    dry_run: Option<bool>,
    /// Seconds to wait for the transfer to end (default 60; 0 returns at once). It goes on by itself.
    wait_seconds: Option<u64>,
}

impl TransferArgs {
    fn options(&self) -> TransferOptions {
        TransferOptions {
            dry_run: self.dry_run.unwrap_or(false),
            wait: wait(self.wait_seconds),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ShowTransferArgs {
    /// The transfer id that `copy` or `move` returned.
    id: String,
    /// Seconds to wait for the transfer to end (default 60; 0 reports how it stands now).
    wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ConfirmReviewArgs {
    /// The sync id.
    id: String,
    /// The `filters_hash` that `show_review` returned.
    filters_hash: String,
    /// Seconds to wait for the round to end (default 60; 0 returns at once). The round goes on by itself.
    wait_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum KeepArg {
    /// Delete the conflicted copy.
    Current,
    /// The conflicted copy becomes the file; the current one goes to the saved versions.
    Conflicted,
    /// Keep the conflicted copy beside the file under a new name.
    Both,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResolveConflictArgs {
    /// The sync id.
    id: String,
    /// The conflicted copy, relative to the sync's folder, as `list_conflicts` shows it.
    path: String,
    /// Which version to keep.
    keep: KeepArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RoundLogArgs {
    /// The sync id.
    id: String,
    /// The round, as `list_activity` shows it.
    round: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ThemeArgs {
    /// `system`, `light`, `dark` or a file of the themes folder. Absent: only report the current one.
    name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SettingsArgs {
    /// The sync id. Absent: the defaults.
    sync: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetSettingArgs {
    /// The sync id. Absent: the defaults.
    sync: Option<String>,
    /// A setting name, as `get_settings` lists it.
    key: String,
    /// The new value: a boolean, a whole number or a string such as `"30s"` or `"newer"`.
    value: serde_json::Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResetSettingArgs {
    /// The sync id. Absent: the defaults.
    sync: Option<String>,
    /// A setting name, as `get_settings` lists it.
    key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BackupTargetArgs {
    /// The backup id, as `list_backups` shows it.
    id: Option<String>,
    /// Every backup, instead of one. Give `id` or `all`, not both.
    all: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BackupIdArgs {
    /// The backup id, as `list_backups` shows it.
    id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RestoreJobArgs {
    /// The restore id that `restore_snapshot` returned.
    id: String,
    /// Seconds to wait for the restore to end. Default 60; 0 answers at once.
    wait_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotsArgs {
    /// The backup id, as `list_backups` shows it.
    id: String,
    /// The most snapshots to list (the newest). All when absent.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotFilesArgs {
    /// The backup id.
    id: String,
    /// `latest`, or a snapshot id (or the start of one) from `list_snapshots`.
    snapshot: String,
    /// A folder of the snapshot, as an earlier answer listed it. Absent: the top.
    dir: Option<String>,
    /// The most entries to list; more are left out and `truncated` says so. Default 200.
    max_entries: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BackupNowArgs {
    /// The backup id.
    id: String,
    /// Seconds to wait for the run to end (default 60; 0 returns at once). The run goes on by itself.
    wait_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CheckArg {
    /// Check the structure of the repository.
    Quick,
    /// Also read a sample of the data.
    Sample,
    /// Read all the data.
    Full,
}

impl From<CheckArg> for CheckKind {
    fn from(kind: CheckArg) -> Self {
        match kind {
            CheckArg::Quick => Self::Quick,
            CheckArg::Sample => Self::Sample,
            CheckArg::Full => Self::Full,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CheckBackupArgs {
    /// The backup id.
    id: String,
    /// How thorough. Default `quick`.
    kind: Option<CheckArg>,
    /// Seconds to wait for the check to end (default 60; 0 returns at once). The check goes on by itself.
    wait_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct KeepArgs {
    /// Daily snapshots to keep.
    daily: Option<i64>,
    /// Weekly snapshots to keep.
    weekly: Option<i64>,
    /// Monthly snapshots to keep.
    monthly: Option<i64>,
    /// Yearly snapshots to keep (0 keeps none).
    yearly: Option<i64>,
}

impl From<KeepArgs> for KeepPartial {
    fn from(keep: KeepArgs) -> Self {
        Self {
            daily: keep.daily,
            weekly: keep.weekly,
            monthly: keep.monthly,
            yearly: keep.yearly,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateBackupArgs {
    /// Name shown for the backup.
    name: String,
    /// Folders on this computer to back up; `~` is the home folder.
    paths: Vec<String>,
    /// A folder on this computer or `account:folder`. It must be empty or new.
    dest: String,
    /// How often: `hourly`, `daily` or `weekly`. Default daily.
    every: Option<String>,
    /// How many snapshots to keep; what is absent has the default.
    keep: Option<KeepArgs>,
    /// Presets to turn on (`system_junk`, `build_artifacts`, ...); the defaults when absent, `[]` turns them off.
    presets: Option<Vec<String>>,
    /// Custom filter rules, each `- pattern` or `+ pattern`.
    filters: Option<Vec<String>>,
    /// Upload limit such as `1M`, or `off`.
    limit_upload: Option<String>,
    /// Download limit such as `1M`, or `off`.
    limit_download: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EditBackupArgs {
    /// The backup id.
    id: String,
    /// A new name.
    name: Option<String>,
    /// The folders to back up, all of them (not only the new one). Removing one is refused.
    paths: Option<Vec<String>>,
    /// The destination is fixed: any change is refused.
    dest: Option<String>,
    /// `true` only; turning it off is refused.
    one_file_system: Option<bool>,
    /// How often: `hourly`, `daily` or `weekly`.
    every: Option<String>,
    /// The snapshots to keep; raising is allowed, lowering is refused.
    keep: Option<KeepArgs>,
    /// The presets to turn on; replaces the current ones.
    presets: Option<Vec<String>>,
    /// Custom filter rules; replaces the current ones.
    filters: Option<Vec<String>>,
    /// Upload limit such as `1M`, or `off`.
    limit_upload: Option<String>,
    /// Download limit such as `1M`, or `off`.
    limit_download: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RestoreSnapshotArgs {
    /// The backup id.
    id: String,
    /// `latest`, or a snapshot id (or the start of one).
    snapshot: String,
    /// The folder to create. It must not exist.
    to: String,
    /// Folders or files of the snapshot to restore. Absent: everything.
    paths: Option<Vec<String>>,
    /// Only report the files and bytes that would be restored and change nothing.
    dry_run: Option<bool>,
    /// Seconds to wait for the restore to end (default 60; 0 returns at once). It goes on by itself.
    wait_seconds: Option<u64>,
}

/// The text `Ops::set_setting` reads for a JSON value.
fn setting_text(value: &serde_json::Value) -> Result<String, OpError> {
    match value {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => Ok(value.to_string()),
        _ => Err(OpError::Invalid(
            "value must be a boolean, a whole number or a string".to_owned(),
        )),
    }
}

#[tool_router]
impl SyncfastMcp {
    #[must_use]
    pub fn new(ops: Ops) -> Self {
        Self { ops: Arc::new(ops) }
    }

    #[tool(
        description = "Every sync with its status (up to date, review needed, paused, offline, ...), gate and last success.",
        annotations(read_only_hint = true)
    )]
    async fn status(&self) -> CallToolResult {
        reply(self.ops.status().await)
    }

    #[tool(
        description = "One sync: folders, filters, status, gate, the filters hash and the counters of the last round.",
        annotations(read_only_hint = true)
    )]
    async fn show_sync(&self, Parameters(args): Parameters<IdArgs>) -> CallToolResult {
        reply(self.ops.show(&args.id).await)
    }

    #[tool(
        description = "Create a sync between a folder on this computer and a remote folder. It starts held for the review of its first round: nothing is copied until show_review and confirm_review.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn add_sync(&self, Parameters(args): Parameters<AddSyncArgs>) -> CallToolResult {
        reply(
            self.ops
                .add(NewSync {
                    name: args.name,
                    local: args.local,
                    remote: args.remote,
                    id: args.id,
                    presets: args.presets,
                    filters: args.filters.unwrap_or_default(),
                    create_local: args.create_local.unwrap_or(false),
                    create_remote: args.create_remote.unwrap_or(false),
                })
                .await,
        )
    }

    #[tool(
        description = "Remove a sync from syncfast. Files on both sides stay. delete_versions also deletes the saved versions of replaced local files.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn remove_sync(&self, Parameters(args): Parameters<RemoveSyncArgs>) -> CallToolResult {
        reply(
            self.ops
                .remove(&args.id, args.delete_versions.unwrap_or(false))
                .await,
        )
    }

    #[tool(
        description = "Pause one sync, or all of them. A paused sync does not run.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn pause_sync(&self, Parameters(args): Parameters<OneOrAllArgs>) -> CallToolResult {
        match one_or_all(args.id, args.all, "sync") {
            Ok(id) => reply(self.ops.set_enabled(id.as_deref(), false).await),
            Err(err) => reply::<()>(Err(err)),
        }
    }

    #[tool(
        description = "Resume a paused sync, or all of them.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn resume_sync(&self, Parameters(args): Parameters<OneOrAllArgs>) -> CallToolResult {
        match one_or_all(args.id, args.all, "sync") {
            Ok(id) => reply(self.ops.set_enabled(id.as_deref(), true).await),
            Err(err) => reply::<()>(Err(err)),
        }
    }

    #[tool(
        description = "Set the presets and custom rules of a sync; what is not given stays. A rule is a line starting with `- ` (exclude) or `+ ` (include). Changed filters hold the sync for a new review.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn set_filters(&self, Parameters(args): Parameters<SetFiltersArgs>) -> CallToolResult {
        reply(
            self.ops
                .set_filters(&args.id, args.presets, args.rules)
                .await,
        )
    }

    #[tool(
        description = "Count the files that filters would leave out of a sync folder (and the top folders they are in), without changing anything. Say how many files a rule excludes before saving it.",
        annotations(read_only_hint = true)
    )]
    async fn test_filters(&self, Parameters(args): Parameters<TestFiltersArgs>) -> CallToolResult {
        reply(
            self.ops
                .test_filters(
                    args.id.as_deref(),
                    args.local.as_deref(),
                    args.presets,
                    args.rules,
                )
                .await,
        )
    }

    #[tool(
        description = "The accounts rclone knows: name, type, and a warning when a Google Drive account uses rclone's shared client ID. Never secrets. Creating an account is for the user.",
        annotations(read_only_hint = true)
    )]
    async fn list_accounts(&self) -> CallToolResult {
        reply(self.ops.accounts().await)
    }

    #[tool(
        description = "The folders directly inside a remote folder (`account:folder`; `account:` is the root).",
        annotations(read_only_hint = true)
    )]
    async fn list_remote_folders(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        reply(self.ops.folders(&args.path).await)
    }

    #[tool(
        description = "Create a remote folder (`account:folder`) and the missing ones above it. Only when the user agreed to it.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn create_remote_folder(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        reply(self.ops.make_folder(&args.path).await)
    }

    #[tool(
        description = "Summarize a folder, on this computer or remote (`account:folder`): for the folder and each subfolder down to `depth`, the number of files, total size, most common extensions and newest date. Names, sizes and dates only, never file content. Large remotes take a while; explore a subfolder when the whole is big.",
        annotations(read_only_hint = true)
    )]
    async fn explore(&self, Parameters(args): Parameters<ExploreArgs>) -> CallToolResult {
        reply(
            self.ops
                .explore(
                    &args.target,
                    ExploreOptions {
                        depth: args.depth.unwrap_or(DEFAULT_DEPTH),
                        max_folders: args.max_folders.unwrap_or(DEFAULT_MAX_FOLDERS),
                    },
                )
                .await,
        )
    }

    #[tool(
        description = "Start a round of a sync (or of all enabled ones) and wait for it up to wait_seconds. A sync held for a review does not run until it is confirmed. The round goes on by itself when the wait ends: read status later.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn sync_now(&self, Parameters(args): Parameters<SyncNowArgs>) -> CallToolResult {
        match one_or_all(args.id, args.all, "sync") {
            Ok(id) => reply(
                self.ops
                    .sync_now(id.as_deref(), wait(args.wait_seconds))
                    .await,
            ),
            Err(err) => reply::<()>(Err(err)),
        }
    }

    #[tool(
        description = "Copy a file or folder to a new place, each side on this computer or `account:path`, in any direction. The destination is the new name and must not exist. Use dry_run to see the files and bytes first. Runs detached and waits up to wait_seconds: when it is still running the answer has finished=false and an id for show_transfer. Refused when the destination exists, when the source is an account root or is or contains the folder of a sync, and while a round of a sync that holds the source or destination runs.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn copy(&self, Parameters(args): Parameters<TransferArgs>) -> CallToolResult {
        reply(
            self.ops
                .copy(&args.source, &args.destination, args.options())
                .await,
        )
    }

    #[tool(
        name = "move",
        description = "Move a file or folder to a new place, like copy, and delete the source once it was transferred without error (a rename on the server inside one account). Only when the user asked for it; dry_run first. A failed move leaves what was not transferred at the source. Same refusals, arguments and answer as copy.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn move_files(&self, Parameters(args): Parameters<TransferArgs>) -> CallToolResult {
        reply(
            self.ops
                .r#move(&args.source, &args.destination, args.options())
                .await,
        )
    }

    #[tool(
        description = "How a copy or move stands, by the id it returned: running, done or failed (with the error). Waits up to wait_seconds for it to end.",
        annotations(read_only_hint = true)
    )]
    async fn show_transfer(
        &self,
        Parameters(args): Parameters<ShowTransferArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .transfer_status(&args.id, wait(args.wait_seconds))
                .await,
        )
    }

    #[tool(
        description = "Dry-run the review a held sync waits for and list what would change (uploads, downloads, replacements, deletions). Returns the filters_hash that confirm_review needs, and whether confirm_review will accept it: it does not when the review deletes files.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn show_review(&self, Parameters(args): Parameters<IdArgs>) -> CallToolResult {
        reply(self.ops.show_review(&args.id).await)
    }

    #[tool(
        description = "Confirm the review that show_review showed, and start the sync it releases. Refused with `This review deletes files. Confirm it in syncfast.` when the review deletes files, and when filters_hash is not the one that was shown or the filters changed since. Nothing changes when it is refused.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn confirm_review(
        &self,
        Parameters(args): Parameters<ConfirmReviewArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .confirm_review(&args.id, &args.filters_hash, wait(args.wait_seconds))
                .await,
        )
    }

    #[tool(
        description = "The conflicts of a sync: files that changed on both sides, with the loser saved as `<name>.conflictN` (or `<name>.path1`/`<name>.path2` when the sync names losers by path). Each has its size and date, current and conflicted.",
        annotations(read_only_hint = true)
    )]
    async fn list_conflicts(&self, Parameters(args): Parameters<IdArgs>) -> CallToolResult {
        reply(self.ops.conflicts(&args.id).await)
    }

    #[tool(
        description = "Settle one conflict: keep the current file, keep the conflicted copy (the current one goes to the saved versions) or keep both (the copy is renamed). Starts a sync of the result.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn resolve_conflict(
        &self,
        Parameters(args): Parameters<ResolveConflictArgs>,
    ) -> CallToolResult {
        let keep = match args.keep {
            KeepArg::Current => Keep::Current,
            KeepArg::Conflicted => Keep::Conflicted,
            KeepArg::Both => Keep::Both,
        };
        reply(
            self.ops
                .resolve_conflict(&args.id, &PathBuf::from(args.path), keep)
                .await,
        )
    }

    #[tool(
        description = "The recent rounds of a sync, newest first: when, how long, result, and counts of uploads, downloads and deletions.",
        annotations(read_only_hint = true)
    )]
    async fn list_activity(&self, Parameters(args): Parameters<IdArgs>) -> CallToolResult {
        reply(self.ops.activity(&args.id).await)
    }

    #[tool(
        description = "What one round logged: its last messages, oldest first. The round is a name from list_activity.",
        annotations(read_only_hint = true)
    )]
    async fn show_round_log(&self, Parameters(args): Parameters<RoundLogArgs>) -> CallToolResult {
        reply(self.ops.round_log(&args.id, &args.round).await)
    }

    #[tool(
        description = "Report the window theme, or set it when `name` is given (`system`, `light`, `dark` or a theme file).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn theme(&self, Parameters(args): Parameters<ThemeArgs>) -> CallToolResult {
        match args.name {
            Some(name) => reply(self.ops.set_theme(&name).await),
            None => reply::<_>(Ok::<_, OpError>(self.ops.theme().await)),
        }
    }

    #[tool(
        description = "Every behavior setting with its value, where it comes from (`builtin`, `defaults` or `sync`), the value it falls back to on reset, and the keys whose value is less safe than the built-in default. With `sync`, the settings of that sync; without it, the defaults every sync inherits.",
        annotations(read_only_hint = true)
    )]
    async fn get_settings(&self, Parameters(args): Parameters<SettingsArgs>) -> CallToolResult {
        reply(self.ops.settings(args.sync.as_deref()).await)
    }

    #[tool(
        description = "Set one behavior setting of a sync, or of the defaults when `sync` is absent. `value` is a JSON boolean, whole number or string (`\"30s\"`, `\"newer\"`). Refused with `This setting lowers safety (...). Change it in syncfast.` when it raises max_delete, turns versions off, sets conflict_loser to delete, switches the mode to a mirror or moves review towards automatic: those are for the user. Nothing changes when it is refused.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn set_setting(&self, Parameters(args): Parameters<SetSettingArgs>) -> CallToolResult {
        match setting_text(&args.value) {
            Ok(value) => reply(
                self.ops
                    .set_setting(args.sync.as_deref(), &args.key, &value)
                    .await,
            ),
            Err(err) => reply::<()>(Err(err)),
        }
    }

    #[tool(
        description = "Remove the own value of a setting of a sync, or the changed default when `sync` is absent, so it falls back to the inherited or built-in value. Refused like set_setting when the value it falls back to is less safe.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn reset_setting(
        &self,
        Parameters(args): Parameters<ResetSettingArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .reset_setting(args.sync.as_deref(), &args.key)
                .await,
        )
    }

    #[tool(
        description = "Every backup with its status (up to date, backing up, save the key, ...), gate, last snapshot and whether its key is stored. The key itself is never shown.",
        annotations(read_only_hint = true)
    )]
    async fn list_backups(&self) -> CallToolResult {
        reply(self.ops.backups().await)
    }

    #[tool(
        description = "One backup: folders, destination, schedule, keep, presets, filters, limits, status, gate, and the details of the last run and check.",
        annotations(read_only_hint = true)
    )]
    async fn show_backup(&self, Parameters(args): Parameters<BackupIdArgs>) -> CallToolResult {
        reply(self.ops.show_backup(&args.id).await)
    }

    #[tool(
        description = "The snapshots of a backup, newest first.",
        annotations(read_only_hint = true)
    )]
    async fn list_snapshots(&self, Parameters(args): Parameters<SnapshotsArgs>) -> CallToolResult {
        reply(self.ops.snapshots(&args.id, args.limit).await)
    }

    #[tool(
        description = "The files of one folder of a snapshot: names, kinds, sizes and dates, never content. Without dir, the top of the snapshot.",
        annotations(read_only_hint = true)
    )]
    async fn list_snapshot_files(
        &self,
        Parameters(args): Parameters<SnapshotFilesArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .snapshot_files(
                    &args.id,
                    &args.snapshot,
                    args.dir.as_deref().unwrap_or(""),
                    args.max_entries.unwrap_or(DEFAULT_FILE_LIMIT),
                )
                .await,
        )
    }

    #[tool(
        description = "Back up now and wait for it. A run that outlasts the wait goes on by itself: read list_backups later.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn backup_now(&self, Parameters(args): Parameters<BackupNowArgs>) -> CallToolResult {
        reply(self.ops.backup_now(&args.id, wait(args.wait_seconds)).await)
    }

    #[tool(
        description = "Check the repository of a backup (quick, sample or full) and wait for it. The result is in show_backup under last_check.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn check_backup(&self, Parameters(args): Parameters<CheckBackupArgs>) -> CallToolResult {
        let kind = args.kind.map_or(CheckKind::Quick, CheckKind::from);
        reply(
            self.ops
                .check_backup(&args.id, kind, wait(args.wait_seconds))
                .await,
        )
    }

    #[tool(
        description = "Pause the schedule of one backup, or of all (all = true). Nothing is deleted.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn pause_backup(&self, Parameters(args): Parameters<BackupTargetArgs>) -> CallToolResult {
        match one_or_all(args.id, args.all, "backup") {
            Ok(id) => reply(self.ops.pause_backup(id.as_deref()).await),
            Err(err) => reply::<()>(Err(err)),
        }
    }

    #[tool(
        description = "Resume the schedule of one paused backup, or of all (all = true).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn resume_backup(
        &self,
        Parameters(args): Parameters<BackupTargetArgs>,
    ) -> CallToolResult {
        match one_or_all(args.id, args.all, "backup") {
            Ok(id) => reply(self.ops.resume_backup(id.as_deref()).await),
            Err(err) => reply::<()>(Err(err)),
        }
    }

    #[tool(
        description = "Create a backup of folders on this computer into an empty or new destination, with a key made and kept by syncfast; you never see it. It starts its first run at once and stays `Save the key` until the user confirms in syncfast that they saved the key: tell them to open syncfast.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn create_backup(
        &self,
        Parameters(args): Parameters<CreateBackupArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .create_backup(NewBackupRequest {
                    name: args.name,
                    paths: args.paths,
                    dest: args.dest,
                    options: BackupOptions {
                        every: args.every,
                        keep: args.keep.map(KeepPartial::from).unwrap_or_default(),
                        presets: args.presets,
                        filters: args.filters,
                        limit_upload: args.limit_upload,
                        limit_download: args.limit_download,
                    },
                })
                .await,
        )
    }

    #[tool(
        description = "Change settings of a backup: name, folders (all of them), every, keep, presets, filters, limit_upload, limit_download, one_file_system on. Refused with `This setting lowers safety (...). Change it in syncfast.` when it keeps fewer snapshots, removes a folder or turns one_file_system off, and refused for good when it changes the destination: those are for the user. Nothing changes when it is refused.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn edit_backup(&self, Parameters(args): Parameters<EditBackupArgs>) -> CallToolResult {
        let edit = BackupEdit {
            name: args.name,
            paths: args.paths,
            dest: args.dest,
            one_file_system: args.one_file_system,
            options: BackupOptions {
                every: args.every,
                keep: args.keep.map(KeepPartial::from).unwrap_or_default(),
                presets: args.presets,
                filters: args.filters,
                limit_upload: args.limit_upload,
                limit_download: args.limit_download,
            },
        };
        reply(self.ops.edit_backup(&args.id, &edit).await)
    }

    #[tool(
        description = "Always refused: only the user removes a backup, in syncfast.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn remove_backup(&self, Parameters(args): Parameters<BackupIdArgs>) -> CallToolResult {
        reply(Ops::remove_backup(&args.id))
    }

    #[tool(
        description = "Restore a snapshot into a folder that does not exist yet (never over existing files), all of it or the given paths. Use dry_run first to show the user the files and bytes. A restore that outlasts the wait goes on by itself: read it with show_backup_job. Returns the folder and the counts, never contents.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn restore_snapshot(
        &self,
        Parameters(args): Parameters<RestoreSnapshotArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .restore(
                    RestoreArgs {
                        backup: args.id,
                        snapshot: args.snapshot,
                        to: args.to,
                        paths: args.paths.unwrap_or_default(),
                        dry_run: args.dry_run.unwrap_or(false),
                    },
                    wait(args.wait_seconds),
                )
                .await,
        )
    }

    #[tool(
        description = "How a restore stands: running or done, with the counts; a restore that failed is an error with the reason. Waits up to wait_seconds for it to end. The id is the one restore_snapshot returned.",
        annotations(read_only_hint = true)
    )]
    async fn show_backup_job(
        &self,
        Parameters(args): Parameters<RestoreJobArgs>,
    ) -> CallToolResult {
        reply(
            self.ops
                .restore_job(&args.id, wait(args.wait_seconds))
                .await,
        )
    }
}

#[allow(clippy::unused_async_trait_impl)]
#[tool_handler]
impl ServerHandler for SyncfastMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("syncfast", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves the tools on stdin and stdout until the client closes them.
///
/// # Errors
/// The transport fails to start or breaks.
pub async fn serve(ops: Ops) -> anyhow::Result<()> {
    let server = SyncfastMcp::new(ops)
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}
