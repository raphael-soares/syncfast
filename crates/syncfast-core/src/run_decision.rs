//! What a round does before it touches rclone: the pure decision table of
//! the spec (step 7). Review is mandatory: only a matching confirmation
//! marker lets a resync or a `--force` run.

use crate::bisync::{Mode, ReviewKind};
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
        return Decision::SetGate(GateKind::LocalMissing);
    }
    let gate = input.gate.filter(|kind| *kind != GateKind::LocalMissing);
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
            GateKind::ConfigError,
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
