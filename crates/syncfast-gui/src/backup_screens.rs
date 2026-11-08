//! What the screens of the selected backup remember between frames, and
//! the loads they ask for: the repository's snapshots when Snapshots is
//! open, the logs when Activity is.

use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;

use crate::backend::{Command, Event};
use crate::backup_runs::BackupRunEntry;
use crate::snapshot::{BackupSnapshot, Snapshot};
use crate::ui::backup_detail::{BackupDetailData, BackupTab, SnapshotsView};
use crate::ui::filters::{FilterRules, FilterTest, FilterTestState};
use crate::ui::settings::KeyStorageView;
use crate::ui::{Action, ViewModel};

/// What a load of the snapshots saw: when it changes, the list is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotsKey {
    id: String,
    last_snapshot: Option<String>,
    running: bool,
}

/// The same for the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunsKey {
    id: String,
    running: bool,
    last_run: Option<Timestamp>,
    last_check: Option<Timestamp>,
}

impl SnapshotsKey {
    fn of(backup: &BackupSnapshot) -> Self {
        Self {
            id: backup.config.id.clone(),
            last_snapshot: backup
                .state
                .last_snapshot
                .as_ref()
                .map(|snapshot| snapshot.id.clone()),
            running: backup.running.is_some(),
        }
    }
}

impl RunsKey {
    fn of(backup: &BackupSnapshot) -> Self {
        Self {
            id: backup.config.id.clone(),
            running: backup.running.is_some(),
            last_run: backup.state.last_run.as_ref().map(|run| run.finished),
            last_check: backup.state.last_check.as_ref().map(|check| check.at),
        }
    }
}

#[derive(Debug, Default)]
pub struct BackupScreens {
    tab: BackupTab,
    snapshots_key: Option<SnapshotsKey>,
    snapshots: SnapshotsView,
    runs_key: Option<RunsKey>,
    runs: Option<Arc<[BackupRunEntry]>>,
    filter_test: Option<FilterTest>,
    saved: Option<(String, bool)>,
    storage: KeyStorageView,
    restic_version: Option<String>,
    restic_asked: bool,
    picking: bool,
    picked: Option<PathBuf>,
}

impl BackupScreens {
    pub fn fill(&self, vm: &mut ViewModel) {
        vm.key_storage = self.storage.clone();
        vm.restic_version.clone_from(&self.restic_version);
        let Some(id) = vm.selected_backup.as_deref() else {
            return;
        };
        let snapshots_shown = self.snapshots_key.as_ref().is_some_and(|key| key.id == id);
        let runs_shown = self.runs_key.as_ref().is_some_and(|key| key.id == id);
        vm.backup = BackupDetailData {
            tab: self.tab,
            snapshots: if snapshots_shown {
                self.snapshots.clone()
            } else {
                SnapshotsView::Loading
            },
            runs: runs_shown.then(|| self.runs.clone()).flatten(),
            filter_test: self
                .filter_test
                .as_ref()
                .filter(|test| test.id == id)
                .cloned(),
            saved: self
                .saved
                .as_ref()
                .filter(|(saved, _)| saved == id)
                .map(|(_, filters_changed)| *filters_changed),
            picked: self.picked.clone(),
        };
    }

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::BackupSnapshots { id, outcome } => {
                if self.snapshots_key.as_ref().is_some_and(|key| key.id == id) {
                    self.snapshots = match outcome {
                        Ok(list) => SnapshotsView::Loaded(list),
                        Err(text) => SnapshotsView::Failed(text),
                    };
                }
            }
            Event::BackupRuns { id, runs } => {
                if self.runs_key.as_ref().is_some_and(|key| key.id == id) {
                    self.runs = Some(runs);
                }
            }
            Event::BackupFilterTest {
                id,
                presets,
                rules,
                outcome,
            } => {
                let rules = FilterRules {
                    presets,
                    custom: rules,
                };
                if self
                    .filter_test
                    .as_ref()
                    .is_some_and(|test| test.id == id && test.rules == rules)
                {
                    let state = match outcome {
                        Ok(count) => FilterTestState::Counted(count),
                        Err(text) => FilterTestState::Failed(text),
                    };
                    self.filter_test = Some(FilterTest { id, rules, state });
                }
            }
            Event::BackupSettingsSaved {
                id,
                filters_changed,
            } => self.saved = Some((id, filters_changed)),
            Event::KeyStorage { test, outcome } => {
                self.storage = match (test, outcome) {
                    (false, Ok(backend)) => KeyStorageView::Found(backend),
                    (false, Err(text)) => KeyStorageView::Missing(text),
                    (true, Ok(backend)) => KeyStorageView::Tested(backend),
                    (true, Err(text)) => KeyStorageView::Failed(text),
                };
            }
            Event::ResticVersion(version) => {
                self.restic_asked = true;
                self.restic_version = version;
            }
            Event::FolderPicked(picked) => {
                self.picking = false;
                self.picked = picked;
            }
            _ => {}
        }
    }

    /// The Settings tab asked for a folder and the answer has not come.
    #[must_use]
    pub fn picking(&self) -> bool {
        self.picking
    }

    /// Settings > Backups was opened: look at the key storage again and
    /// read the restic version once.
    pub fn enter_settings(&mut self) -> Vec<Command> {
        self.storage = KeyStorageView::Checking;
        let mut commands = vec![Command::CheckKeyStorage { test: false }];
        if !self.restic_asked {
            self.restic_asked = true;
            commands.push(Command::LoadResticVersion);
        }
        commands
    }

    /// The loads the open tab of the selected backup needs, when what it
    /// shows is stale.
    pub fn refresh(&mut self, snapshot: &Snapshot, selected: Option<&str>) -> Vec<Command> {
        let Some(backup) = selected.and_then(|id| {
            snapshot
                .backups
                .iter()
                .find(|backup| backup.config.id == id)
        }) else {
            return Vec::new();
        };
        let id = backup.config.id.clone();
        match self.tab {
            BackupTab::Snapshots => {
                let key = SnapshotsKey::of(backup);
                if self.snapshots_key.as_ref() == Some(&key) {
                    return Vec::new();
                }
                if self.snapshots_key.as_ref().is_none_or(|old| old.id != id) {
                    self.snapshots = SnapshotsView::Loading;
                }
                self.snapshots_key = Some(key);
                vec![Command::LoadBackupSnapshots(id)]
            }
            BackupTab::Activity => {
                let key = RunsKey::of(backup);
                if self.runs_key.as_ref() == Some(&key) {
                    return Vec::new();
                }
                if self.runs_key.as_ref().is_none_or(|old| old.id != id) {
                    self.runs = None;
                }
                let running = key.running;
                self.runs_key = Some(key);
                vec![Command::LoadBackupRuns { id, running }]
            }
            BackupTab::Settings => Vec::new(),
        }
    }

    /// The share of the actions that belong to these screens; the command
    /// to send, if any.
    pub fn act(&mut self, action: Action) -> Option<Command> {
        match action {
            Action::SelectBackupTab(tab) => {
                self.tab = tab;
                None
            }
            Action::RefreshBackupSnapshots(id) => {
                if self.snapshots_key.as_ref().is_some_and(|key| key.id == id) {
                    self.snapshots = SnapshotsView::Loading;
                    self.snapshots_key = None;
                }
                None
            }
            Action::TestBackupFilters { backup } => {
                self.filter_test = Some(FilterTest {
                    id: backup.id.clone(),
                    rules: FilterRules {
                        presets: backup.presets.clone(),
                        custom: backup.filters.clone(),
                    },
                    state: FilterTestState::Running,
                });
                Some(Command::TestBackupFilters { backup })
            }
            Action::SaveBackupSettings {
                backup,
                filters_changed,
            } => {
                self.saved = None;
                Some(Command::SaveBackupSettings {
                    backup,
                    filters_changed,
                })
            }
            Action::DismissBackupSaved(id) => {
                if self.saved.as_ref().is_some_and(|(saved, _)| *saved == id) {
                    self.saved = None;
                }
                None
            }
            Action::TestKeyStorage => {
                self.storage = KeyStorageView::Checking;
                Some(Command::CheckKeyStorage { test: true })
            }
            Action::PickBackupFolder => {
                self.picking = true;
                self.picked = None;
                Some(Command::PickFolder)
            }
            Action::TakeBackupFolder => {
                self.picked = None;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use syncfast_core::backup::keystore::KeyBackend;
    use syncfast_core::backup::state::{BackupState, LastSnapshot};
    use syncfast_core::backup::status::BackupStatus;
    use syncfast_core::config::Config;

    use super::*;

    fn snapshot(last_snapshot: Option<&str>) -> Snapshot {
        let text = "version = 3\n\n[[backup]]\nid = \"docs\"\nname = \"Documents\"\npaths = [\"/work/docs\"]\ndest = \"/backups/docs\"\n";
        let config = Config::parse(text, Path::new("/home/ada"))
            .unwrap()
            .backups()[0]
            .clone();
        let mut state = BackupState::default();
        if let Some(id) = last_snapshot {
            state.last_snapshot = Some(LastSnapshot {
                id: id.to_owned(),
                time: Timestamp::UNIX_EPOCH,
            });
        }
        Snapshot {
            backups: vec![BackupSnapshot {
                config,
                state,
                running: None,
                status: BackupStatus::UpToDate,
                progress: None,
                missing_folders: Vec::new(),
            }],
            ..Snapshot::default()
        }
    }

    fn shown(screens: &BackupScreens, id: &str) -> ViewModel {
        let mut vm = ViewModel {
            selected_backup: Some(id.to_owned()),
            ..ViewModel::default()
        };
        screens.fill(&mut vm);
        vm
    }

    #[test]
    fn the_snapshots_load_once_until_the_newest_snapshot_changes() {
        let mut screens = BackupScreens::default();

        let first = screens.refresh(&snapshot(Some("a")), Some("docs"));
        let again = screens.refresh(&snapshot(Some("a")), Some("docs"));
        let after_a_backup = screens.refresh(&snapshot(Some("b")), Some("docs"));

        assert!(matches!(first[..], [Command::LoadBackupSnapshots(_)]));
        assert!(again.is_empty());
        assert!(matches!(
            after_a_backup[..],
            [Command::LoadBackupSnapshots(_)]
        ));
    }

    #[test]
    fn nothing_loads_for_an_unknown_or_unselected_backup() {
        let mut screens = BackupScreens::default();

        assert!(screens.refresh(&snapshot(None), None).is_empty());
        assert!(screens.refresh(&snapshot(None), Some("other")).is_empty());
    }

    #[test]
    fn the_loaded_snapshots_reach_the_view_model_of_that_backup_only() {
        let mut screens = BackupScreens::default();
        screens.refresh(&snapshot(None), Some("docs"));
        screens.on_event(Event::BackupSnapshots {
            id: "docs".to_owned(),
            outcome: Ok(Arc::from(Vec::new())),
        });

        assert!(matches!(
            shown(&screens, "docs").backup.snapshots,
            SnapshotsView::Loaded(_)
        ));
        assert!(matches!(
            shown(&screens, "other").backup.snapshots,
            SnapshotsView::Loading
        ));
    }

    #[test]
    fn a_late_answer_for_another_backup_is_dropped() {
        let mut screens = BackupScreens::default();
        screens.refresh(&snapshot(None), Some("docs"));

        screens.on_event(Event::BackupSnapshots {
            id: "other".to_owned(),
            outcome: Err("boom".to_owned()),
        });

        assert!(matches!(
            shown(&screens, "docs").backup.snapshots,
            SnapshotsView::Loading
        ));
    }

    #[test]
    fn a_failed_load_is_shown_and_try_again_loads_again() {
        let mut screens = BackupScreens::default();
        screens.refresh(&snapshot(None), Some("docs"));
        screens.on_event(Event::BackupSnapshots {
            id: "docs".to_owned(),
            outcome: Err("repository locked".to_owned()),
        });
        let failed = shown(&screens, "docs").backup.snapshots;

        screens.act(Action::RefreshBackupSnapshots("docs".to_owned()));
        let commands = screens.refresh(&snapshot(None), Some("docs"));

        assert!(matches!(failed, SnapshotsView::Failed(text) if text == "repository locked"));
        assert!(matches!(commands[..], [Command::LoadBackupSnapshots(_)]));
    }

    #[test]
    fn the_activity_tab_loads_the_logs_and_the_snapshots_tab_does_not() {
        let mut screens = BackupScreens::default();
        screens.act(Action::SelectBackupTab(BackupTab::Activity));

        let commands = screens.refresh(&snapshot(None), Some("docs"));

        assert!(matches!(
            commands[..],
            [Command::LoadBackupRuns { running: false, .. }]
        ));
    }

    #[test]
    fn testing_the_key_storage_shows_checking_until_the_answer() {
        let mut screens = BackupScreens::default();

        let command = screens.act(Action::TestKeyStorage);
        let waiting = shown(&screens, "docs").key_storage;
        screens.on_event(Event::KeyStorage {
            test: true,
            outcome: Ok(KeyBackend::Portal),
        });
        let passed = shown(&screens, "docs").key_storage;
        screens.on_event(Event::KeyStorage {
            test: true,
            outcome: Err("the portal refused the item".to_owned()),
        });
        let failed = shown(&screens, "docs").key_storage;

        assert!(matches!(
            command,
            Some(Command::CheckKeyStorage { test: true })
        ));
        assert_eq!(waiting, KeyStorageView::Checking);
        assert_eq!(passed, KeyStorageView::Tested(KeyBackend::Portal));
        assert_eq!(
            failed,
            KeyStorageView::Failed("the portal refused the item".to_owned())
        );
    }

    #[test]
    fn a_look_that_is_not_a_test_still_says_where_the_keys_are_or_that_none_answered() {
        let mut screens = BackupScreens::default();

        screens.on_event(Event::KeyStorage {
            test: false,
            outcome: Ok(KeyBackend::HostKeyring),
        });
        let found = shown(&screens, "docs").key_storage;
        screens.on_event(Event::KeyStorage {
            test: false,
            outcome: Err("none".to_owned()),
        });
        let missing = shown(&screens, "docs").key_storage;

        assert_eq!(found, KeyStorageView::Found(KeyBackend::HostKeyring));
        assert_eq!(missing, KeyStorageView::Missing("none".to_owned()));
    }

    #[test]
    fn opening_the_settings_reads_the_restic_version_only_once() {
        let mut screens = BackupScreens::default();

        let first = screens.enter_settings();
        let second = screens.enter_settings();

        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn a_picked_folder_is_kept_until_the_form_takes_it() {
        let mut screens = BackupScreens::default();
        screens.act(Action::PickBackupFolder);
        let picking = screens.picking();

        screens.on_event(Event::FolderPicked(Some(PathBuf::from("/work/new"))));
        let picked = shown(&screens, "docs").backup.picked;
        screens.act(Action::TakeBackupFolder);

        assert!(picking);
        assert!(!screens.picking());
        assert_eq!(picked, Some(PathBuf::from("/work/new")));
        assert_eq!(shown(&screens, "docs").backup.picked, None);
    }
}
