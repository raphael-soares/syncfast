//! What the Conflicts and Filters tabs and the `Remove Sync` alert remember
//! between frames, and what they ask of the backend.

use jiff::Timestamp;
use syncfast_core::conflicts::Conflict;

use crate::backend::{Command, Event};
use crate::snapshot::Snapshot;
use crate::ui::filters::{FilterRules, FilterTest, FilterTestState};
use crate::ui::{Action, ViewModel};

/// What tells that a round changed the conflicts of a sync.
type ConflictKey = (String, Option<Timestamp>, u32);

#[derive(Debug, Default)]
pub struct Screens {
    pub settings_open: bool,
    removing: Option<String>,
    conflicts: Option<(String, Vec<Conflict>)>,
    conflict_key: Option<ConflictKey>,
    filter_test: Option<FilterTest>,
    filters_saved: Option<String>,
}

impl Screens {
    /// Puts what these screens show into the view model.
    pub fn fill(&self, vm: &mut ViewModel) {
        vm.removing.clone_from(&self.removing);
        vm.conflicts = self
            .conflicts
            .as_ref()
            .filter(|(id, _)| Some(id) == vm.selected.as_ref())
            .map(|(_, found)| found.clone())
            .unwrap_or_default();
        vm.filter_test.clone_from(&self.filter_test);
        vm.filters_saved = self.filters_saved.is_some() && self.filters_saved == vm.selected;
    }

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Conflicts { id, found } => self.conflicts = Some((id, found)),
            Event::FilterTest {
                id,
                presets,
                rules,
                outcome,
            } => {
                self.filter_test = Some(FilterTest {
                    id,
                    rules: FilterRules {
                        presets,
                        custom: rules,
                    },
                    state: match outcome {
                        Ok(summary) => FilterTestState::Done(summary),
                        Err(message) => FilterTestState::Failed(message),
                    },
                });
            }
            Event::FiltersSaved(id) => self.filters_saved = Some(id),
            _ => {}
        }
    }

    /// A new snapshot: when the selected sync was just synced or the number
    /// of its conflicts changed, look at its folder again.
    pub fn on_snapshot(&mut self, snapshot: &Snapshot, selected: Option<&str>) -> Option<Command> {
        let key = snapshot
            .pairs
            .iter()
            .find(|pair| Some(pair.pair.id.as_str()) == selected)
            .map(|pair| {
                (
                    pair.pair.id.clone(),
                    pair.state.last_success,
                    pair.state.conflicts_pending,
                )
            });
        if key == self.conflict_key {
            return None;
        }
        self.conflict_key.clone_from(&key);
        match key {
            Some((id, _, pending)) if pending > 0 => Some(Command::ScanConflicts(id)),
            _ => {
                self.conflicts = None;
                None
            }
        }
    }

    /// The backend command an action of these screens means, if any.
    pub fn act(&mut self, action: Action) -> Option<Command> {
        match action {
            Action::RemoveSync(id) => {
                self.removing = Some(id);
                None
            }
            Action::CancelRemove => {
                self.removing = None;
                None
            }
            Action::ConfirmRemove {
                id,
                delete_versions,
            } => {
                self.removing = None;
                Some(Command::RemoveSync {
                    id,
                    delete_versions,
                })
            }
            Action::RefreshConflicts(id) => Some(Command::ScanConflicts(id)),
            Action::ResolveConflict {
                id,
                path,
                resolution,
            } => Some(Command::ResolveConflict {
                id,
                path,
                resolution,
            }),
            Action::TestFilters {
                id,
                local,
                presets,
                rules,
            } => {
                self.filter_test = Some(FilterTest {
                    id: id.clone(),
                    rules: FilterRules {
                        presets: presets.clone(),
                        custom: rules.clone(),
                    },
                    state: FilterTestState::Running,
                });
                Some(Command::TestFilters {
                    id,
                    local,
                    presets,
                    rules,
                })
            }
            Action::SaveFilters { id, presets, rules } => {
                self.filters_saved = None;
                Some(Command::SaveFilters { id, presets, rules })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use syncfast_core::config::Pair;
    use syncfast_core::state::{DisplayStatus, State};

    use super::*;
    use crate::snapshot::PairSnapshot;

    fn snapshot(id: &str, conflicts_pending: u32, last_success: Option<&str>) -> Snapshot {
        Snapshot {
            config_error: None,
            pairs: vec![PairSnapshot {
                pair: Pair {
                    id: id.to_owned(),
                    name: id.to_owned(),
                    local: PathBuf::from("/data"),
                    remote: "r:".to_owned(),
                    enabled: true,
                    presets: Vec::new(),
                    filters: Vec::new(),
                },
                state: State {
                    conflicts_pending,
                    last_success: last_success.map(|at| at.parse().unwrap()),
                    ..State::default()
                },
                running: false,
                status: DisplayStatus::UpToDate,
                progress: None,
            }],
        }
    }

    #[test]
    fn a_selected_sync_with_pending_conflicts_is_scanned_once_per_change() {
        let mut screens = Screens::default();
        let with_two = snapshot("notes", 2, Some("2026-11-03T10:00:00Z"));

        let first = screens.on_snapshot(&with_two, Some("notes"));
        let repeated = screens.on_snapshot(&with_two, Some("notes"));
        let next_round = screens.on_snapshot(
            &snapshot("notes", 2, Some("2026-11-03T10:05:00Z")),
            Some("notes"),
        );

        assert_eq!(first, Some(Command::ScanConflicts("notes".to_owned())));
        assert_eq!(repeated, None);
        assert_eq!(next_round, Some(Command::ScanConflicts("notes".to_owned())));
    }

    #[test]
    fn a_sync_without_pending_conflicts_is_not_scanned_and_forgets_the_old_list() {
        let mut screens = Screens::default();
        screens.on_event(Event::Conflicts {
            id: "notes".to_owned(),
            found: Vec::new(),
        });

        let command = screens.on_snapshot(&snapshot("notes", 0, None), Some("notes"));

        assert_eq!(command, None);
        assert!(screens.conflicts.is_none());
    }

    #[test]
    fn the_conflicts_of_another_sync_are_not_shown() {
        let mut screens = Screens::default();
        screens.on_event(Event::Conflicts {
            id: "notes".to_owned(),
            found: vec![Conflict {
                path: "a.conflict1".into(),
                original: "a".into(),
                current: None,
                conflicted: syncfast_core::conflicts::FileFacts {
                    size: 1,
                    modified: None,
                },
            }],
        });
        let mut vm = ViewModel {
            selected: Some("work".to_owned()),
            ..ViewModel::default()
        };

        screens.fill(&mut vm);

        assert!(vm.conflicts.is_empty());
    }

    #[test]
    fn confirming_the_removal_closes_the_alert_and_sends_the_choice_on() {
        let mut screens = Screens::default();
        screens.act(Action::RemoveSync("notes".to_owned()));
        let mut vm = ViewModel::default();
        screens.fill(&mut vm);
        assert_eq!(vm.removing.as_deref(), Some("notes"));

        let command = screens.act(Action::ConfirmRemove {
            id: "notes".to_owned(),
            delete_versions: true,
        });
        screens.fill(&mut vm);

        assert_eq!(
            command,
            Some(Command::RemoveSync {
                id: "notes".to_owned(),
                delete_versions: true
            })
        );
        assert_eq!(vm.removing, None);
    }

    #[test]
    fn saving_filters_hides_the_saved_notice_until_the_backend_confirms() {
        let mut screens = Screens::default();
        let mut vm = ViewModel {
            selected: Some("notes".to_owned()),
            ..ViewModel::default()
        };
        screens.on_event(Event::FiltersSaved("notes".to_owned()));
        screens.fill(&mut vm);
        assert!(vm.filters_saved);

        screens.act(Action::SaveFilters {
            id: "notes".to_owned(),
            presets: Vec::new(),
            rules: Vec::new(),
        });
        screens.fill(&mut vm);

        assert!(!vm.filters_saved);
    }
}
