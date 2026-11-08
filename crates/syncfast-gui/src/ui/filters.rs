//! The Filters tab: which files a sync leaves out. Two presets, custom rules
//! with a check per line, a test against the real folder, and `Save`.

use egui::{Align, Id, Layout, RichText, Ui};
use syncfast_core::backup::config::filter_line_problem as backup_filter_line_problem;
use syncfast_core::backup::test_filters::FilterTest as BackupFilterTest;
use syncfast_core::config::filter_line_problem;
use syncfast_core::filters::{ExclusionSummary, PRESETS};

use super::format::bytes;
use super::widgets::{inline_error, primary_button, secondary_button, switch, text_area};
use super::{Action, ViewModel};
use crate::theme::{self, space};

/// The id of a `Test Filters` run from the Add Sync wizard, where the sync
/// does not exist yet.
pub const NEW_SYNC: &str = "";

pub const SAVED: &str = "Saved. The next sync will ask you to review the changes.";

/// The rules as the user has them on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterDraft {
    /// Names of the presets that are on, in the order of [`PRESETS`].
    pub presets: Vec<String>,
    /// One custom rule per line, as typed.
    pub rules: String,
}

impl FilterDraft {
    /// The draft that equals what is saved in the config.
    #[must_use]
    pub fn saved(presets: &[String], custom: &[String]) -> Self {
        Self {
            presets: PRESETS
                .iter()
                .filter(|preset| presets.iter().any(|name| name == preset.name))
                .map(|preset| preset.name.to_owned())
                .collect(),
            rules: custom.join("\n"),
        }
    }

    /// The custom rules to save: the lines that are not blank, without
    /// trailing spaces.
    #[must_use]
    pub fn custom_rules(&self) -> Vec<String> {
        self.rules
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// `(line number, message)` of every line that is not a rule.
    #[must_use]
    pub fn problems(&self) -> Vec<(usize, String)> {
        self.problems_for(FilterTarget::Sync)
    }

    /// [`Self::problems`] for the rules of a sync or of a backup.
    #[must_use]
    pub fn problems_for(&self, target: FilterTarget) -> Vec<(usize, String)> {
        self.rules
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .filter_map(|(index, line)| {
                let line = line.trim_end();
                match target {
                    FilterTarget::Sync => filter_line_problem(index + 1, line),
                    FilterTarget::Backup => backup_filter_line_problem(index + 1, line),
                }
                .map(|text| (index + 1, text))
            })
            .collect()
    }

    /// Whether both hold the same rules once saved: blank lines and trailing
    /// spaces do not count.
    #[must_use]
    pub fn same_rules(&self, other: &Self) -> bool {
        self.presets == other.presets && self.custom_rules() == other.custom_rules()
    }

    fn is_on(&self, name: &str) -> bool {
        self.presets.iter().any(|on| on == name)
    }

    fn set(&mut self, name: &str, on: bool) {
        self.presets.retain(|preset| preset != name);
        if on {
            self.presets.push(name.to_owned());
            self.presets
                .sort_by_key(|name| PRESETS.iter().position(|preset| preset.name == name));
        }
    }
}

/// The rules a test ran with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterRules {
    pub presets: Vec<String>,
    pub custom: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterTestState {
    Running,
    Done(ExclusionSummary),
    /// The count of a backup's test: files and bytes, no folder list.
    Counted(BackupFilterTest),
    Failed(String),
}

/// Whose rules the editor shows: backups read them with restic, which
/// accepts fewer patterns and has its own warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterTarget {
    Sync,
    Backup,
}

/// The last `Test Filters` of a sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterTest {
    pub id: String,
    pub rules: FilterRules,
    pub state: FilterTestState,
}

const BUILD_ARTIFACTS_NOTES: [&str; 2] = [
    "Leaves out node_modules, target, .venv and caches. They can be rebuilt.",
    "Build folders (build/, dist/) are not part of this preset. Add them as custom rules if they only hold output.",
];

fn preset_title(name: &str) -> &str {
    match name {
        "system_junk" => "System junk files",
        "dev_dirs" => "Developer folders",
        "build_artifacts" => "Build artifacts and caches",
        "git_repos" => "Git repositories",
        other => other,
    }
}

/// The filters of the selected sync, with their own draft that lives until
/// it equals what is saved.
pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let Some(pair) = vm.selected_pair() else {
        return Vec::new();
    };
    let ctx = ui.ctx().clone();
    let saved = FilterDraft::saved(&pair.presets, &pair.filters);
    let key = Id::new(("filters-draft", &pair.id));
    let mut draft = ctx
        .data(|data| data.get_temp::<FilterDraft>(key))
        .unwrap_or_else(|| saved.clone());
    let dirty = !draft.same_rules(&saved);
    let problems = draft.problems();
    let mut actions = Vec::new();
    let save = Action::SaveFilters {
        id: pair.id.clone(),
        presets: draft.presets.clone(),
        rules: draft.custom_rules(),
    };
    let tested = vm
        .filter_test
        .as_ref()
        .filter(|test| test.id == pair.id && test.rules == rules_of(&draft));
    let test_clicked = form(ui, &pair.id, &mut draft, tested, |ui| {
        ui.add_enabled_ui(dirty && problems.is_empty(), |ui| {
            if primary_button(ui, "Save").clicked() {
                actions.push(save);
            }
        });
    });
    let dirty_now = !draft.same_rules(&saved);
    if test_clicked {
        actions.push(Action::TestFilters {
            id: pair.id.clone(),
            local: pair.local.clone(),
            presets: draft.presets.clone(),
            rules: draft.custom_rules(),
        });
    }
    if vm.filters_saved && dirty_now {
        actions.push(Action::DismissFiltersSaved(pair.id.clone()));
    }
    if vm.filters_saved && !dirty_now {
        ui.label(
            RichText::new(SAVED)
                .font(theme::callout())
                .color(theme::palette(ui.ctx()).secondary),
        );
    }
    // Only an exact match drops the draft: a trailing Enter is the user
    // still typing, so it must survive until the next frame.
    ctx.data_mut(|data| {
        if draft == saved {
            data.remove::<FilterDraft>(key);
        } else {
            data.insert_temp(key, draft);
        }
    });
    actions
}

fn rules_of(draft: &FilterDraft) -> FilterRules {
    FilterRules {
        presets: draft.presets.clone(),
        custom: draft.custom_rules(),
    }
}

/// The editor of a sync: preset switches with their rules, the custom rules,
/// and a `Test Filters` button with its result. `trailing` draws at the right
/// end of the button row. Returns whether `Test Filters` was clicked.
pub fn form(
    ui: &mut Ui,
    salt: &str,
    draft: &mut FilterDraft,
    test: Option<&FilterTest>,
    trailing: impl FnOnce(&mut Ui),
) -> bool {
    form_for(ui, salt, draft, test, FilterTarget::Sync, trailing)
}

fn backup_warning(preset: &str) -> Option<&'static str> {
    match preset {
        "dev_dirs" => {
            Some("This also leaves out .git folders, so repository history is not backed up.")
        }
        "git_repos" => Some("This leaves out every folder that holds a .git."),
        _ => None,
    }
}

/// [`form`] for the rules of a sync or of a backup.
pub fn form_for(
    ui: &mut Ui,
    salt: &str,
    draft: &mut FilterDraft,
    test: Option<&FilterTest>,
    target: FilterTarget,
    trailing: impl FnOnce(&mut Ui),
) -> bool {
    let palette = theme::palette(ui.ctx());
    ui.spacing_mut().item_spacing.y = space::M;
    for preset in &PRESETS {
        let mut on = draft.is_on(preset.name);
        if switch(ui, &mut on, preset_title(preset.name)).changed() {
            draft.set(preset.name, on);
        }
        if target == FilterTarget::Backup
            && on
            && let Some(warning) = backup_warning(preset.name)
        {
            ui.label(
                RichText::new(warning)
                    .font(theme::callout())
                    .color(palette.warning),
            );
        }
        if preset.rules.is_empty() {
            let found = if target == FilterTarget::Backup {
                "backup"
            } else {
                "sync"
            };
            ui.label(
                RichText::new(format!(
                    "Leaves out every folder with a .git inside, found on each {found}"
                ))
                .font(theme::callout())
                .color(palette.secondary),
            );
            continue;
        }
        if preset.name == "build_artifacts" {
            for note in BUILD_ARTIFACTS_NOTES {
                ui.label(
                    RichText::new(note)
                        .font(theme::callout())
                        .color(palette.secondary),
                );
            }
        }
        egui::CollapsingHeader::new(
            RichText::new("Show rules")
                .font(theme::callout())
                .color(palette.secondary),
        )
        .id_salt((salt, preset.name))
        .show(ui, |ui| {
            for rule in preset.rules {
                ui.label(
                    RichText::new(*rule)
                        .font(theme::monospace())
                        .color(palette.secondary),
                );
            }
        });
    }
    text_area(
        ui,
        "Custom rules",
        &mut draft.rules,
        6,
        "One rule per line, such as - *.bak",
    );
    let problems = draft.problems_for(target);
    for (_, message) in &problems {
        inline_error(ui, message);
    }
    let mut test_clicked = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(problems.is_empty(), |ui| {
            test_clicked = secondary_button(ui, "Test Filters").clicked();
        });
        ui.with_layout(Layout::right_to_left(Align::Center), trailing);
    });
    if let Some(test) = test {
        outcome(ui, &test.state);
    }
    test_clicked
}

fn outcome(ui: &mut Ui, state: &FilterTestState) {
    let palette = theme::palette(ui.ctx());
    match state {
        FilterTestState::Running => {
            ui.label(
                RichText::new("Testing…")
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
        FilterTestState::Failed(message) => {
            inline_error(ui, message);
        }
        FilterTestState::Counted(count) => {
            ui.label(
                RichText::new(format!(
                    "{} of {} files excluded \u{b7} {}",
                    count.excluded_files,
                    count.total_files,
                    bytes(count.excluded_bytes)
                ))
                .font(theme::headline())
                .color(palette.text),
            );
        }
        FilterTestState::Done(summary) => {
            ui.label(
                RichText::new(format!(
                    "{} of {} files excluded",
                    summary.excluded, summary.total
                ))
                .font(theme::headline())
                .color(palette.text),
            );
            for (folder, count) in &summary.folders {
                ui.label(
                    RichText::new(format!("{folder} · {count} files"))
                        .font(theme::callout())
                        .color(palette.secondary),
                );
            }
            if !summary.git_repos.is_empty() {
                ui.label(
                    RichText::new("Git repositories left out")
                        .font(theme::callout())
                        .color(palette.text),
                );
                for repo in &summary.git_repos {
                    ui.label(
                        RichText::new(format!("{repo}/"))
                            .font(theme::callout())
                            .color(palette.secondary),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn the_saved_draft_orders_the_presets_and_drops_unknown_ones() {
        let draft = FilterDraft::saved(&names(&["dev_dirs", "nope", "system_junk"]), &[]);

        assert_eq!(draft.presets, ["system_junk", "dev_dirs"]);
    }

    #[test]
    fn turning_a_preset_on_puts_it_back_in_order() {
        let mut draft = FilterDraft::saved(&names(&["dev_dirs"]), &[]);

        draft.set("system_junk", true);

        assert_eq!(draft.presets, ["system_junk", "dev_dirs"]);
        draft.set("dev_dirs", false);
        assert_eq!(draft.presets, ["system_junk"]);
    }

    #[test]
    fn blank_lines_are_not_rules_and_trailing_spaces_are_cut() {
        let draft = FilterDraft {
            presets: Vec::new(),
            rules: "- *.bak  \n\n   \n+ keep/**".to_owned(),
        };

        assert_eq!(draft.custom_rules(), ["- *.bak", "+ keep/**"]);
        assert!(draft.problems().is_empty());
    }

    #[test]
    fn a_line_without_a_sign_is_a_problem_at_its_own_line_number() {
        let draft = FilterDraft {
            presets: Vec::new(),
            rules: "- *.bak\n\n*.tmp".to_owned(),
        };

        let problems = draft.problems();

        assert_eq!(
            problems,
            [(
                3,
                "filter line 3 must start with \"+ \" or \"- \"".to_owned()
            )]
        );
    }
}
