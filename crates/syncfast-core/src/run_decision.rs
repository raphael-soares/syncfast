//! What a round does before it touches rclone: the pure decision table of
//! the spec (step 7). Review is mandatory: only a matching confirmation
//! marker lets a resync or a `--force` run.

use crate::behavior::ReviewLevel;
use crate::bisync::{Mode, ReviewKind};
use crate::preview::ChangeCounts;
use crate::state::{ConfirmMarker, GateKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Paused,
    Offline,
    Gated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Skip(SkipReason),
    SetGate(GateKind),
    /// Only ever `Normal`, `Resync` or `Force`.
    Run(Mode),
}

impl Decision {
    /// The marker is dropped once the decision gets past the early exits
    /// (paused, no local folder, offline) without consuming it.
    #[must_use]
    pub fn discards_marker(self) -> bool {
        !matches!(
            self,
            Self::Skip(SkipReason::Paused | SkipReason::Offline)
                | Self::SetGate(GateKind::LocalMissing)
                | Self::Run(Mode::Resync | Mode::Force)
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DecideInput<'a> {
    pub pair_enabled: bool,
    pub local_exists: bool,
    pub online: bool,
    pub gate: Option<GateKind>,
    pub applied_filters_hash: Option<&'a str>,
    pub current_filters_hash: &'a str,
    pub marker: Option<&'a ConfirmMarker>,
}

/// The first row of the table that matches wins.
#[must_use]
pub fn decide(input: &DecideInput<'_>) -> Decision {
    if !input.pair_enabled {
        return Decision::Skip(SkipReason::Paused);
    }
    if !input.local_exists {
        // Another gate keeps its place: a review still pending must survive
        // the folder going away and coming back. Attention gives way: it
        // blocks nothing and the next round finds the failure again.
        return if matches!(
            input.gate,
            None | Some(GateKind::LocalMissing | GateKind::NeedsAttention)
        ) {
            Decision::SetGate(GateKind::LocalMissing)
        } else {
            Decision::Skip(SkipReason::Gated)
        };
    }
    let resync_confirmed = input.marker.is_some_and(|marker| {
        marker.kind == ReviewKind::Resync && marker.filters_hash == input.current_filters_hash
    });
    let gate = match input.gate {
        // The first review was confirmed and its round failed: run it again
        // instead of hiding the failure behind a new review.
        Some(GateKind::NeedsAttention)
            if input.applied_filters_hash.is_none() && resync_confirmed =>
        {
            Some(GateKind::NeedsReviewFirstRun)
        }
        // The pair is valid again (or its failure may be gone). One that
        // never synced still owes its first review.
        Some(GateKind::ConfigError | GateKind::NeedsAttention)
            if input.applied_filters_hash.is_none() =>
        {
            return Decision::SetGate(GateKind::NeedsReviewFirstRun);
        }
        // Attention never stops rounds: the next one may succeed.
        Some(GateKind::LocalMissing | GateKind::ConfigError | GateKind::NeedsAttention) => None,
        other => other,
    };
    if !input.online {
        return Decision::Skip(SkipReason::Offline);
    }
    if let Some(marker) = input
        .marker
        .filter(|marker| marker.filters_hash == input.current_filters_hash)
    {
        match (marker.kind, gate) {
            (ReviewKind::Resync, Some(kind)) if kind.is_needs_review() => {
                return Decision::Run(Mode::Resync);
            }
            (ReviewKind::Force, Some(kind)) if kind.is_force_review() => {
                return Decision::Run(Mode::Force);
            }
            _ => {}
        }
    }
    if gate.is_some() {
        return Decision::Skip(SkipReason::Gated);
    }
    if input
        .applied_filters_hash
        .is_some_and(|applied| applied != input.current_filters_hash)
    {
        return Decision::SetGate(GateKind::NeedsReviewFiltersChanged);
    }
    Decision::Run(Mode::Normal)
}

/// What the review level makes of a gate that waits for a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReview {
    Confirm,
    Hold,
}

/// Whether a dry run could make `level` confirm `gate` by itself. False
/// for the gates no preview decides: they hold at every level, and so does
/// the first review of a sync unless the level is `automatic`.
#[must_use]
pub fn may_confirm(level: ReviewLevel, gate: GateKind) -> bool {
    let reviewable = matches!(
        gate,
        GateKind::NeedsReviewFirstRun
            | GateKind::NeedsReviewFiltersChanged
            | GateKind::TooManyDeletes
            | GateKind::AllFilesChanged
    );
    match level {
        ReviewLevel::Manual => false,
        ReviewLevel::Deletions => reviewable && gate != GateKind::NeedsReviewFirstRun,
        ReviewLevel::Automatic => reviewable,
    }
}

/// The review table of the spec (step 4), over the preview of the review
/// `gate` waits for.
#[must_use]
pub fn auto_review(level: ReviewLevel, gate: GateKind, counts: &ChangeCounts) -> AutoReview {
    let deletes_block = level == ReviewLevel::Deletions && counts.deletes();
    if may_confirm(level, gate) && !deletes_block {
        AutoReview::Confirm
    } else {
        AutoReview::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "hash-now";

    fn input() -> DecideInput<'static> {
        DecideInput {
            pair_enabled: true,
            local_exists: true,
            online: true,
            gate: None,
            applied_filters_hash: Some(HASH),
            current_filters_hash: HASH,
            marker: None,
        }
    }

    fn marker(kind: ReviewKind, hash: &str) -> ConfirmMarker {
        ConfirmMarker {
            kind,
            filters_hash: hash.to_owned(),
        }
    }

    #[test]
    fn paused_pair_is_skipped_even_when_everything_else_blocks_it() {
        let decision = decide(&DecideInput {
            pair_enabled: false,
            local_exists: false,
            online: false,
            gate: Some(GateKind::TooManyDeletes),
            ..input()
        });

        assert_eq!(decision, Decision::Skip(SkipReason::Paused));
    }

    #[test]
    fn missing_local_folder_sets_the_local_missing_gate() {
        let decision = decide(&DecideInput {
            local_exists: false,
            online: false,
            ..input()
        });

        assert_eq!(decision, Decision::SetGate(GateKind::LocalMissing));
    }

    #[test]
    fn local_missing_gate_is_ignored_once_the_folder_is_back() {
        let decision = decide(&DecideInput {
            gate: Some(GateKind::LocalMissing),
            ..input()
        });

        assert_eq!(decision, Decision::Run(Mode::Normal));
    }

    #[test]
    fn an_attention_gate_does_not_stop_rounds() {
        let decision = decide(&DecideInput {
            gate: Some(GateKind::NeedsAttention),
            ..input()
        });

        assert_eq!(decision, Decision::Run(Mode::Normal));
    }

    #[test]
    fn an_attention_gate_of_a_pair_that_never_synced_goes_back_to_its_first_review() {
        let decision = decide(&DecideInput {
            gate: Some(GateKind::NeedsAttention),
            applied_filters_hash: None,
            ..input()
        });

        assert_eq!(decision, Decision::SetGate(GateKind::NeedsReviewFirstRun));
    }

    #[test]
    fn an_attention_gate_of_a_never_synced_pair_resumes_its_confirmed_resync() {
        let confirmed = marker(ReviewKind::Resync, HASH);

        let decision = decide(&DecideInput {
            gate: Some(GateKind::NeedsAttention),
            applied_filters_hash: None,
            marker: Some(&confirmed),
            ..input()
        });

        assert_eq!(decision, Decision::Run(Mode::Resync));
        assert!(!decision.discards_marker());
    }

    #[test]
    fn an_attention_gate_of_a_never_synced_pair_ignores_a_marker_for_other_filters_or_a_force() {
        for stale in [
            marker(ReviewKind::Resync, "old-hash"),
            marker(ReviewKind::Force, HASH),
        ] {
            let decision = decide(&DecideInput {
                gate: Some(GateKind::NeedsAttention),
                applied_filters_hash: None,
                marker: Some(&stale),
                ..input()
            });

            assert_eq!(decision, Decision::SetGate(GateKind::NeedsReviewFirstRun));
        }
    }

    #[test]
    fn an_attention_gate_of_a_never_synced_pair_waits_offline_for_its_confirmed_resync() {
        let confirmed = marker(ReviewKind::Resync, HASH);

        let decision = decide(&DecideInput {
            online: false,
            gate: Some(GateKind::NeedsAttention),
            applied_filters_hash: None,
            marker: Some(&confirmed),
            ..input()
        });

        assert_eq!(decision, Decision::Skip(SkipReason::Offline));
    }

    #[test]
    fn a_missing_local_folder_replaces_an_attention_gate() {
        let decision = decide(&DecideInput {
            local_exists: false,
            gate: Some(GateKind::NeedsAttention),
            ..input()
        });

        assert_eq!(decision, Decision::SetGate(GateKind::LocalMissing));
    }

    #[test]
    fn offline_pair_is_skipped_before_any_marker_is_considered() {
        let confirmed = marker(ReviewKind::Resync, HASH);

        let decision = decide(&DecideInput {
            online: false,
            gate: Some(GateKind::NeedsReviewFirstRun),
            marker: Some(&confirmed),
            ..input()
        });

        assert_eq!(decision, Decision::Skip(SkipReason::Offline));
    }

    #[test]
    fn resync_marker_with_matching_hash_runs_a_resync_for_every_review_gate() {
        let confirmed = marker(ReviewKind::Resync, HASH);
        for gate in [
            GateKind::NeedsReviewFirstRun,
            GateKind::NeedsReviewFiltersChanged,
            GateKind::NeedsReviewCriticalError,
        ] {
            let decision = decide(&DecideInput {
                gate: Some(gate),
                applied_filters_hash: None,
                marker: Some(&confirmed),
                ..input()
            });

            assert_eq!(decision, Decision::Run(Mode::Resync), "{gate:?}");
        }
    }

    #[test]
    fn force_marker_with_matching_hash_runs_force_on_every_force_review_gate() {
        let confirmed = marker(ReviewKind::Force, HASH);
        for gate in [GateKind::TooManyDeletes, GateKind::AllFilesChanged] {
            let decision = decide(&DecideInput {
                gate: Some(gate),
                marker: Some(&confirmed),
                ..input()
            });

            assert_eq!(decision, Decision::Run(Mode::Force), "{gate:?}");
        }
    }

    #[test]
    fn marker_that_matches_neither_resync_nor_force_is_discarded_and_the_gate_holds() {
        let cases = [
            (marker(ReviewKind::Resync, HASH), GateKind::TooManyDeletes),
            (
                marker(ReviewKind::Force, HASH),
                GateKind::NeedsReviewFirstRun,
            ),
            (
                marker(ReviewKind::Resync, "old-hash"),
                GateKind::NeedsReviewFirstRun,
            ),
            (
                marker(ReviewKind::Force, "old-hash"),
                GateKind::TooManyDeletes,
            ),
            (marker(ReviewKind::Resync, HASH), GateKind::AuthError),
            (marker(ReviewKind::Resync, HASH), GateKind::AllFilesChanged),
            (
                marker(ReviewKind::Force, "old-hash"),
                GateKind::AllFilesChanged,
            ),
        ];
        for (stale, gate) in cases {
            let decision = decide(&DecideInput {
                gate: Some(gate),
                marker: Some(&stale),
                ..input()
            });

            assert_eq!(
                decision,
                Decision::Skip(SkipReason::Gated),
                "{stale:?} {gate:?}"
            );
            assert!(decision.discards_marker());
        }
    }

    #[test]
    fn marker_without_a_gate_is_discarded_and_the_round_runs_normally() {
        let confirmed = marker(ReviewKind::Resync, HASH);

        let decision = decide(&DecideInput {
            marker: Some(&confirmed),
            ..input()
        });

        assert_eq!(decision, Decision::Run(Mode::Normal));
        assert!(decision.discards_marker());
    }

    #[test]
    fn any_gate_without_a_marker_skips_the_round() {
        for gate in [
            GateKind::NeedsReviewFirstRun,
            GateKind::TooManyDeletes,
            GateKind::AllFilesChanged,
            GateKind::AuthError,
        ] {
            let decision = decide(&DecideInput {
                gate: Some(gate),
                ..input()
            });

            assert_eq!(decision, Decision::Skip(SkipReason::Gated), "{gate:?}");
        }
    }

    #[test]
    fn review_gate_is_kept_while_the_local_folder_is_missing() {
        for gate in [
            GateKind::NeedsReviewFirstRun,
            GateKind::NeedsReviewFiltersChanged,
            GateKind::NeedsReviewCriticalError,
            GateKind::TooManyDeletes,
            GateKind::AllFilesChanged,
            GateKind::AuthError,
            GateKind::ConfigError,
        ] {
            let decision = decide(&DecideInput {
                local_exists: false,
                gate: Some(gate),
                ..input()
            });

            assert_eq!(decision, Decision::Skip(SkipReason::Gated), "{gate:?}");
        }
    }

    #[test]
    fn local_missing_gate_stays_set_while_the_folder_is_still_missing() {
        let decision = decide(&DecideInput {
            local_exists: false,
            gate: Some(GateKind::LocalMissing),
            ..input()
        });

        assert_eq!(decision, Decision::SetGate(GateKind::LocalMissing));
    }

    #[test]
    fn config_error_gate_of_a_valid_pair_is_ignored_once_it_has_synced() {
        let decision = decide(&DecideInput {
            gate: Some(GateKind::ConfigError),
            ..input()
        });

        assert_eq!(decision, Decision::Run(Mode::Normal));
    }

    #[test]
    fn config_error_gate_of_a_valid_pair_that_never_synced_asks_for_the_first_review() {
        let decision = decide(&DecideInput {
            gate: Some(GateKind::ConfigError),
            applied_filters_hash: None,
            ..input()
        });

        assert_eq!(decision, Decision::SetGate(GateKind::NeedsReviewFirstRun));
    }

    #[test]
    fn changed_filters_after_a_successful_round_set_the_filters_changed_gate() {
        let decision = decide(&DecideInput {
            applied_filters_hash: Some("hash-before"),
            ..input()
        });

        assert_eq!(
            decision,
            Decision::SetGate(GateKind::NeedsReviewFiltersChanged)
        );
    }

    #[test]
    fn filters_never_applied_do_not_set_the_filters_changed_gate() {
        let decision = decide(&DecideInput {
            applied_filters_hash: None,
            ..input()
        });

        assert_eq!(decision, Decision::Run(Mode::Normal));
    }

    #[test]
    fn healthy_pair_runs_normally() {
        assert_eq!(decide(&input()), Decision::Run(Mode::Normal));
    }

    #[test]
    fn an_auto_confirmed_first_run_runs_as_resync_without_a_gate_notice() {
        let confirmed = marker(ReviewKind::Resync, HASH);
        let never_synced = DecideInput {
            applied_filters_hash: None,
            ..input()
        };

        for (gate, held) in [
            (
                GateKind::NeedsReviewFirstRun,
                Decision::Skip(SkipReason::Gated),
            ),
            (
                GateKind::ConfigError,
                Decision::SetGate(GateKind::NeedsReviewFirstRun),
            ),
        ] {
            let waiting = DecideInput {
                gate: Some(gate),
                ..never_synced
            };
            let released = DecideInput {
                gate: Some(GateKind::NeedsReviewFirstRun),
                marker: Some(&confirmed),
                ..never_synced
            };

            assert_eq!(decide(&waiting), held, "{gate:?}");
            assert_eq!(decide(&released), Decision::Run(Mode::Resync), "{gate:?}");
        }
    }

    fn counts(delete_local: u32, delete_remote: u32) -> ChangeCounts {
        ChangeCounts {
            upload: 4,
            overwrite_remote: 2,
            delete_local,
            delete_remote,
            ..ChangeCounts::default()
        }
    }

    #[test]
    fn auto_review_follows_the_table_of_levels_gates_and_deletions() {
        use AutoReview::{Confirm, Hold};
        use GateKind::{
            AllFilesChanged, NeedsReviewFiltersChanged, NeedsReviewFirstRun, TooManyDeletes,
        };
        use ReviewLevel::{Automatic, Deletions, Manual};

        let nothing = counts(0, 0);
        let local = counts(1, 0);
        let remote = counts(0, 3);
        let table = [
            (Manual, NeedsReviewFirstRun, nothing, Hold),
            (Manual, NeedsReviewFiltersChanged, nothing, Hold),
            (Manual, AllFilesChanged, nothing, Hold),
            (Manual, TooManyDeletes, nothing, Hold),
            (Deletions, NeedsReviewFirstRun, nothing, Hold),
            (Deletions, NeedsReviewFiltersChanged, nothing, Confirm),
            (Deletions, NeedsReviewFiltersChanged, local, Hold),
            (Deletions, NeedsReviewFiltersChanged, remote, Hold),
            (Deletions, AllFilesChanged, nothing, Confirm),
            (Deletions, AllFilesChanged, remote, Hold),
            (Deletions, TooManyDeletes, nothing, Confirm),
            (Deletions, TooManyDeletes, local, Hold),
            (Deletions, TooManyDeletes, remote, Hold),
            (Automatic, NeedsReviewFirstRun, nothing, Confirm),
            (Automatic, NeedsReviewFiltersChanged, remote, Confirm),
            (Automatic, AllFilesChanged, local, Confirm),
            (Automatic, TooManyDeletes, remote, Confirm),
            (Automatic, TooManyDeletes, nothing, Confirm),
        ];

        for (level, gate, counts, expected) in table {
            assert_eq!(
                auto_review(level, gate, &counts),
                expected,
                "{level:?} {gate:?} {counts:?}"
            );
        }
    }

    #[test]
    fn a_critical_error_is_held_at_every_level() {
        for level in [
            ReviewLevel::Manual,
            ReviewLevel::Deletions,
            ReviewLevel::Automatic,
        ] {
            assert_eq!(
                auto_review(level, GateKind::NeedsReviewCriticalError, &counts(0, 0)),
                AutoReview::Hold,
                "{level:?}"
            );
            assert!(!may_confirm(level, GateKind::NeedsReviewCriticalError));
        }
    }

    #[test]
    fn gates_that_are_not_reviews_are_never_confirmed() {
        for level in [
            ReviewLevel::Manual,
            ReviewLevel::Deletions,
            ReviewLevel::Automatic,
        ] {
            for gate in [
                GateKind::NeedsAttention,
                GateKind::AuthError,
                GateKind::ConfigError,
                GateKind::LocalMissing,
            ] {
                assert_eq!(
                    auto_review(level, gate, &counts(0, 0)),
                    AutoReview::Hold,
                    "{level:?} {gate:?}"
                );
            }
        }
    }

    #[test]
    fn force_marker_does_not_survive_a_filter_change_and_the_gate_holds() {
        let confirmed = marker(ReviewKind::Force, "hash-before");

        let decision = decide(&DecideInput {
            gate: Some(GateKind::TooManyDeletes),
            marker: Some(&confirmed),
            ..input()
        });

        assert_eq!(decision, Decision::Skip(SkipReason::Gated));
    }

    #[test]
    fn review_gate_survives_pause_and_resume() {
        let gated = |pair_enabled| {
            decide(&DecideInput {
                pair_enabled,
                gate: Some(GateKind::NeedsReviewFirstRun),
                applied_filters_hash: None,
                ..input()
            })
        };

        assert_eq!(gated(false), Decision::Skip(SkipReason::Paused));
        assert_eq!(gated(true), Decision::Skip(SkipReason::Gated));
    }

    #[test]
    fn early_exits_and_consuming_runs_keep_the_marker() {
        for decision in [
            Decision::Skip(SkipReason::Paused),
            Decision::Skip(SkipReason::Offline),
            Decision::SetGate(GateKind::LocalMissing),
            Decision::Run(Mode::Resync),
            Decision::Run(Mode::Force),
        ] {
            assert!(!decision.discards_marker(), "{decision:?}");
        }
    }
}
