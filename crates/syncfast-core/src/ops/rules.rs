//! The filters of a sync: saving them and seeing what they leave out.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;

use super::{DEFAULT_WAIT_SECONDS, OpError, Ops, PairView};
use crate::config::{Config, Pair, expand_tilde, filter_line_problem};
use crate::filters::{self, ExclusionSummary, PRESETS};
use crate::paths::Paths;
use crate::state::{Gate, GateKind, State};

static TEST_FILES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FiltersSaved {
    pub sync: PairView,
    pub filters_hash: String,
    /// The new filters differ from the ones the last sync used, so the
    /// next round asks for a review.
    pub review_needed: bool,
    pub message: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilterTestView {
    pub total: usize,
    pub excluded: usize,
    /// `<x> of <y> files excluded`.
    pub summary: String,
    /// The top-level folders with the most excluded files, most first.
    pub folders: Vec<FolderCount>,
    /// Folders left out because they hold a `.git`.
    pub git_repos: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderCount {
    pub folder: String,
    pub excluded_files: usize,
}

fn check_rules(presets: &[String], rules: &[String]) -> Result<(), OpError> {
    for name in presets {
        if filters::preset(name).is_none() {
            return Err(OpError::Invalid(format!("unknown preset \"{name}\"")));
        }
    }
    for (index, line) in rules.iter().enumerate() {
        if let Some(problem) = filter_line_problem(index + 1, line) {
            return Err(OpError::Invalid(problem));
        }
    }
    Ok(())
}

/// After the filters changed: a sync held for too many deletions or for
/// every file changing is held for the review of the new filters instead,
/// since the old confirmation no longer applies. Returns whether the sync
/// had no gate at all.
///
/// # Errors
/// The state of the sync cannot be read or written.
pub async fn after_filters_saved(paths: &Paths, id: &str) -> Result<bool, OpError> {
    let mut state = State::load(paths, id).await?;
    match state.gate.as_ref().map(|gate| gate.kind) {
        None => Ok(true),
        Some(kind) if kind.is_force_review() => {
            state.gate = Some(Gate {
                kind: GateKind::NeedsReviewFiltersChanged,
                since: Timestamp::now(),
                detail: String::new(),
                fault: None,
                auto_checked: None,
            });
            state.save(paths, id).await?;
            Ok(false)
        }
        Some(_) => Ok(false),
    }
}

impl Ops {
    /// Writes the presets and custom rules of the sync into the config.
    /// What is not given stays as it is. A sync held for a mass deletion
    /// is held for the review of the new filters instead.
    ///
    /// # Errors
    /// No such sync, an unknown preset or a rule that does not start with
    /// `+ ` or `- `, or the config cannot be saved.
    pub async fn set_filters(
        &self,
        id: &str,
        presets: Option<Vec<String>>,
        rules: Option<Vec<String>>,
    ) -> Result<FiltersSaved, OpError> {
        let current = self.find_pair(id).await?;
        let presets = presets.unwrap_or(current.presets);
        let rules = rules.unwrap_or(current.filters);
        check_rules(&presets, &rules)?;
        if !Config::edit(&self.paths, |config| {
            config.set_filters(id, &presets, &rules)
        })
        .await?
        {
            return Err(OpError::NotFound(id.to_owned()));
        }
        let saved = self.find_pair(id).await?;
        let hash = filters::hash(&filters::render_live(&saved).await.text);
        let before = State::load(&self.paths, id).await?;
        let was_clear = after_filters_saved(&self.paths, id).await?;
        let warnings = self.reapply(&[]).await;
        let review_needed = before.applied_filters_hash.as_deref() != Some(hash.as_str());
        if was_clear && review_needed {
            // A round only sees the new hash and sets the gate; it copies
            // nothing, so it ends at once.
            self.start_round(id, Duration::from_secs(DEFAULT_WAIT_SECONDS))
                .await?;
        }
        Ok(FiltersSaved {
            sync: self.pair_view(id).await?,
            filters_hash: hash,
            review_needed,
            message: if review_needed {
                "Saved. The next sync will ask you to review the changes.".to_owned()
            } else {
                "Saved. The filters are the same as in the last sync.".to_owned()
            },
            warnings,
        })
    }

    /// Lists the files of a folder with and without the rules and says
    /// what they leave out. Give a sync `id` to test its folder and rules,
    /// or `local` (and the rules) to test a folder before there is a sync.
    /// `presets` and `rules` replace the sync's when given.
    ///
    /// # Errors
    /// No such sync, neither `id` nor `local`, an invalid rule, or rclone
    /// cannot list the folder.
    pub async fn test_filters(
        &self,
        id: Option<&str>,
        local: Option<&str>,
        presets: Option<Vec<String>>,
        rules: Option<Vec<String>>,
    ) -> Result<FilterTestView, OpError> {
        let (folder, presets, rules) = match (id, local) {
            (Some(id), _) => {
                let pair = self.find_pair(id).await?;
                (
                    pair.local,
                    presets.unwrap_or(pair.presets),
                    rules.unwrap_or(pair.filters),
                )
            }
            (None, Some(local)) => (
                expand_tilde(Path::new(local.trim()), &self.paths.home),
                presets.unwrap_or_else(|| {
                    PRESETS
                        .iter()
                        .map(|preset| preset.name.to_owned())
                        .collect()
                }),
                rules.unwrap_or_default(),
            ),
            (None, None) => {
                return Err(OpError::Invalid(
                    "give a sync id or a local folder to test".to_owned(),
                ));
            }
        };
        check_rules(&presets, &rules)?;
        let found = self.exclusions(&folder, presets, rules).await?;
        Ok(FilterTestView {
            summary: format!("{} of {} files excluded", found.excluded, found.total),
            total: found.total,
            excluded: found.excluded,
            folders: found
                .folders
                .into_iter()
                .map(|(folder, excluded_files)| FolderCount {
                    folder,
                    excluded_files,
                })
                .collect(),
            git_repos: found.git_repos,
        })
    }

    async fn exclusions(
        &self,
        local: &Path,
        presets: Vec<String>,
        rules: Vec<String>,
    ) -> Result<ExclusionSummary, OpError> {
        let folder = local
            .to_str()
            .ok_or_else(|| OpError::Invalid("the folder's path is not valid text".to_owned()))?;
        let filters::Rendered {
            text: rendered,
            git_repos,
        } = filters::render_live(&Pair {
            id: "test".to_owned(),
            name: "test".to_owned(),
            local: local.to_owned(),
            remote: "test:".to_owned(),
            enabled: true,
            presets,
            filters: rules,
            overrides: crate::settings::Overrides::default(),
        })
        .await;
        let dir = self.paths.state_dir().join("filters");
        let write_failed =
            |err: std::io::Error| OpError::Failed(format!("cannot write the test filters: {err}"));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(write_failed)?;
        let file = dir.join(format!(
            "test-{}-{}.txt",
            std::process::id(),
            TEST_FILES.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::write(&file, rendered)
            .await
            .map_err(write_failed)?;
        let listed = self.list_both(folder, &file).await;
        if let Err(err) = tokio::fs::remove_file(&file).await {
            log::warn!("cannot delete {}: {err}", file.display());
        }
        let (all, kept) = listed?;
        Ok(filters::summarize_exclusions(&all, &kept, git_repos))
    }

    async fn list_both(
        &self,
        folder: &str,
        filters: &Path,
    ) -> Result<(Vec<String>, Vec<String>), OpError> {
        let all = self.rclone.list_files(folder, None).await?;
        let kept = self.rclone.list_files(folder, Some(filters)).await?;
        Ok((all, kept))
    }
}
