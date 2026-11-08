//! What `restic check --json` said (spec step 8): the repository is fine,
//! has damaged data, is locked, or the check itself did not finish.

use serde::Deserialize;

use crate::restic::{EXIT_LOCKED, failure_message};

/// The longest part of restic's first complaint that is kept as detail.
const DETAIL_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Ok,
    Damaged {
        errors: u64,
        broken_packs: Vec<String>,
        /// The first complaint of restic, cut to one line.
        detail: String,
    },
    /// Another process holds the repository.
    Locked,
    /// The check did not get to say anything about the data: no network, a
    /// signal, a failure of the destination.
    Failed(String),
}

impl CheckOutcome {
    /// The files to tell the user about: the broken packs, or the errors
    /// when restic named no pack.
    #[must_use]
    pub fn damaged_files(&self) -> u64 {
        match self {
            Self::Damaged {
                errors,
                broken_packs,
                ..
            } if broken_packs.is_empty() => *errors,
            Self::Damaged { broken_packs, .. } => broken_packs.len() as u64,
            _ => 0,
        }
    }
}

#[derive(Deserialize)]
struct Line {
    message_type: String,
    #[serde(default)]
    num_errors: u64,
    #[serde(default)]
    broken_packs: Option<Vec<String>>,
    #[serde(default)]
    message: String,
}

/// Reads the end of one `restic check --json`. `exit_code` is `None` when a
/// signal ended it.
#[must_use]
pub fn parse_check(exit_code: Option<i32>, stdout: &str, stderr: &str) -> CheckOutcome {
    if exit_code == Some(EXIT_LOCKED) {
        return CheckOutcome::Locked;
    }
    let summary = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Line>(line).ok())
        .find(|line| line.message_type == "summary");
    let broken_packs = summary
        .as_ref()
        .and_then(|summary| summary.broken_packs.clone())
        .unwrap_or_default();
    let errors = summary.as_ref().map_or(0, |summary| summary.num_errors);
    if errors > 0 || (exit_code == Some(1) && !broken_packs.is_empty()) {
        return CheckOutcome::Damaged {
            errors,
            broken_packs,
            detail: first_complaint(stderr),
        };
    }
    if exit_code == Some(0) && summary.is_some() {
        return CheckOutcome::Ok;
    }
    let message = failure_message(stderr);
    CheckOutcome::Failed(if message.is_empty() {
        match exit_code {
            Some(code) => format!("exit code {code}"),
            None => "killed by a signal".to_owned(),
        }
    } else {
        message
    })
}

/// The first `error` line of stderr, its first line of text.
fn first_complaint(stderr: &str) -> String {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Line>(line).ok())
        .find(|line| line.message_type == "error" && !line.message.trim().is_empty())
        .map(|line| line.message)
        .or_else(|| {
            stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .and_then(|text| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
        .chars()
        .take(DETAIL_CHARS)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (Option<i32>, String, String) {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/restic-0.19.1");
        let read = |ext: &str| std::fs::read_to_string(dir.join(format!("{name}.{ext}"))).unwrap();
        (
            Some(read("exit").trim().parse().unwrap()),
            read("out"),
            read("err"),
        )
    }

    fn parse(name: &str) -> CheckOutcome {
        let (code, out, err) = fixture(name);
        parse_check(code, &out, &err)
    }

    #[test]
    fn a_quick_check_without_errors_is_ok() {
        assert_eq!(parse("check_ok"), CheckOutcome::Ok);
    }

    #[test]
    fn a_sample_check_without_errors_is_ok() {
        assert_eq!(parse("check_sample"), CheckOutcome::Ok);
    }

    #[test]
    fn a_damaged_pack_is_named_and_counted() {
        let outcome = parse("check_damaged");

        let CheckOutcome::Damaged {
            errors,
            broken_packs,
            detail,
        } = &outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(*errors, 1);
        assert_eq!(
            broken_packs,
            &["7a18898704954d1b510a7878949315f222f716edf8c0c91a2f1f90c3c916aad0"]
        );
        assert!(detail.starts_with("pack 7a188987"), "{detail}");
        assert!(detail.chars().count() <= DETAIL_CHARS);
        assert!(!detail.contains('\n'));
        assert_eq!(outcome.damaged_files(), 1);
    }

    #[test]
    fn errors_without_a_named_pack_still_make_the_repository_damaged() {
        let out = r#"{"message_type":"summary","num_errors":3,"broken_packs":null}"#;

        let outcome = parse_check(Some(1), out, "Fatal: repository contains errors\n");

        assert_eq!(outcome.damaged_files(), 3);
        assert!(matches!(
            outcome,
            CheckOutcome::Damaged { ref detail, .. } if detail == "Fatal: repository contains errors"
        ));
    }

    #[test]
    fn exit_1_with_broken_packs_is_damaged_even_if_the_error_count_is_missing() {
        let out = r#"{"message_type":"summary","num_errors":0,"broken_packs":["aa","bb"]}"#;

        let outcome = parse_check(Some(1), out, "");

        assert_eq!(outcome.damaged_files(), 2);
    }

    #[test]
    fn broken_packs_without_exit_1_are_not_damage() {
        let out = r#"{"message_type":"summary","num_errors":0,"broken_packs":["aa"]}"#;

        assert_eq!(parse_check(Some(0), out, ""), CheckOutcome::Ok);
    }

    #[test]
    fn exit_11_is_a_lock_whatever_else_was_printed() {
        let err = r#"{"message_type":"exit_error","code":11,"message":"Fatal: unable to create lock in backend: repository is already locked"}"#;

        assert_eq!(parse_check(Some(11), "", err), CheckOutcome::Locked);
        assert_eq!(
            parse_check(
                Some(11),
                "{\"message_type\":\"summary\",\"num_errors\":4}",
                err
            ),
            CheckOutcome::Locked
        );
    }

    #[test]
    fn a_check_that_did_not_finish_says_why() {
        let err = r#"{"message_type":"exit_error","code":1,"message":"Fatal: unable to open repository"}"#;

        assert_eq!(
            parse_check(Some(1), "", err),
            CheckOutcome::Failed("Fatal: unable to open repository".to_owned())
        );
        assert_eq!(
            parse_check(Some(3), "", ""),
            CheckOutcome::Failed("exit code 3".to_owned())
        );
        assert_eq!(
            parse_check(None, "", ""),
            CheckOutcome::Failed("killed by a signal".to_owned())
        );
    }

    #[test]
    fn exit_0_without_a_summary_is_not_a_clean_bill_of_health() {
        assert_eq!(
            parse_check(Some(0), "", ""),
            CheckOutcome::Failed("exit code 0".to_owned())
        );
    }

    #[test]
    fn the_detail_is_cut_to_a_line_of_bounded_length() {
        let long = "x".repeat(DETAIL_CHARS + 50);
        let err = format!("{{\"message_type\":\"error\",\"message\":\"{long}\\nsecond\"}}");

        let outcome = parse_check(
            Some(1),
            r#"{"message_type":"summary","num_errors":1}"#,
            &err,
        );

        let CheckOutcome::Damaged { detail, .. } = outcome else {
            panic!()
        };
        assert_eq!(detail, "x".repeat(DETAIL_CHARS));
    }

    #[test]
    fn nothing_is_damaged_in_an_outcome_that_is_not_damage() {
        assert_eq!(CheckOutcome::Ok.damaged_files(), 0);
        assert_eq!(CheckOutcome::Locked.damaged_files(), 0);
        assert_eq!(CheckOutcome::Failed("x".into()).damaged_files(), 0);
    }
}
