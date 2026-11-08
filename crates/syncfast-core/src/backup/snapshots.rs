//! What `restic snapshots --json` says, as the screens and the CLI show it.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

const SHORT_ID_LEN: usize = 8;

/// One snapshot of a backup's repository. `files`, `bytes` and
/// `added_bytes` are absent for a snapshot restic wrote without a summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotView {
    pub id: String,
    pub short_id: String,
    pub time: Timestamp,
    pub host: String,
    pub paths: Vec<String>,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
    pub added_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct RawSnapshot {
    id: String,
    time: Timestamp,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    hostname: String,
    summary: Option<RawSummary>,
}

#[derive(Deserialize)]
struct RawSummary {
    total_files_processed: Option<u64>,
    total_bytes_processed: Option<u64>,
    data_added: Option<u64>,
}

impl From<RawSnapshot> for SnapshotView {
    fn from(raw: RawSnapshot) -> Self {
        let summary = raw.summary.as_ref();
        Self {
            short_id: raw.id.chars().take(SHORT_ID_LEN).collect(),
            files: summary.and_then(|s| s.total_files_processed),
            bytes: summary.and_then(|s| s.total_bytes_processed),
            added_bytes: summary.and_then(|s| s.data_added),
            id: raw.id,
            time: raw.time,
            host: raw.hostname,
            paths: raw.paths,
        }
    }
}

/// The array `restic snapshots --json` prints, newest first.
///
/// # Errors
/// The text is not that array.
pub fn parse_snapshots(json: &[u8]) -> Result<Vec<SnapshotView>, serde_json::Error> {
    let raw: Vec<RawSnapshot> = serde_json::from_slice(json)?;
    let mut views: Vec<SnapshotView> = raw.into_iter().map(SnapshotView::from).collect();
    views.sort_by_key(|view| std::cmp::Reverse(view.time));
    Ok(views)
}

/// `latest`, or the snapshot whose id is, or starts with, `wanted`.
/// `snapshots` are newest first.
///
/// # Errors
/// The reason, when there is none or the start matches several.
pub fn pick_snapshot<'a>(
    snapshots: &'a [SnapshotView],
    wanted: &str,
) -> Result<&'a SnapshotView, String> {
    if wanted == "latest" {
        return snapshots
            .first()
            .ok_or_else(|| "This backup has no snapshots yet.".to_owned());
    }
    let wanted = wanted.to_ascii_lowercase();
    if wanted.is_empty() {
        return Err("Name a snapshot: latest or an id from `backup snapshots`.".to_owned());
    }
    let mut matches = snapshots
        .iter()
        .filter(|snapshot| snapshot.id.to_ascii_lowercase().starts_with(&wanted));
    match (matches.next(), matches.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(format!("No snapshot has the id \"{wanted}\".")),
        (Some(_), Some(_)) => Err(format!(
            "More than one snapshot starts with \"{wanted}\"; give more of the id."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/restic-0.19.1/snapshots.out");

    #[test]
    fn the_snapshots_fixture_parses_newest_first() {
        let views = parse_snapshots(FIXTURE).unwrap();

        assert_eq!(views.len(), 3);
        assert!(views.windows(2).all(|pair| pair[0].time >= pair[1].time));
    }

    fn snapshot(id: &str, time: &str) -> SnapshotView {
        SnapshotView {
            id: id.to_owned(),
            short_id: id[..8.min(id.len())].to_owned(),
            time: time.parse().unwrap(),
            host: "host".to_owned(),
            paths: vec!["/home/user".to_owned()],
            files: Some(1),
            bytes: Some(1),
            added_bytes: Some(1),
        }
    }

    fn three() -> Vec<SnapshotView> {
        vec![
            snapshot("ab99cc00dd11", "2026-03-01T00:00:00Z"),
            snapshot("cc44dd55ee66", "2026-02-01T00:00:00Z"),
            snapshot("aa11bb22cc33", "2026-01-01T00:00:00Z"),
        ]
    }

    #[test]
    fn latest_is_the_newest_snapshot() {
        assert_eq!(
            pick_snapshot(&three(), "latest").unwrap().id,
            "ab99cc00dd11"
        );
    }

    #[test]
    fn an_id_or_the_start_of_one_picks_the_snapshot_whatever_its_case() {
        let all = three();

        assert_eq!(pick_snapshot(&all, "cc44").unwrap().id, "cc44dd55ee66");
        assert_eq!(
            pick_snapshot(&all, "AA11BB22CC33").unwrap().id,
            "aa11bb22cc33"
        );
    }

    #[test]
    fn a_start_that_fits_two_snapshots_or_none_is_refused() {
        let all = three();

        let ambiguous = pick_snapshot(&all, "a").unwrap_err();
        let unknown = pick_snapshot(&all, "ff").unwrap_err();

        assert!(ambiguous.contains("More than one"), "{ambiguous}");
        assert!(unknown.contains("\"ff\""), "{unknown}");
    }

    #[test]
    fn a_backup_with_no_snapshots_has_no_latest() {
        let err = pick_snapshot(&[], "latest").unwrap_err();

        assert_eq!(err, "This backup has no snapshots yet.");
        assert!(pick_snapshot(&three(), "").is_err());
    }

    #[test]
    fn a_snapshot_carries_its_host_paths_and_counts() {
        let views = parse_snapshots(FIXTURE).unwrap();

        let oldest = views.last().unwrap();
        assert_eq!(oldest.host, "fixture-host");
        assert_eq!(oldest.paths, ["/tmp/fixture/src/docs"]);
        assert_eq!(oldest.files, Some(3));
        assert_eq!(oldest.bytes, Some(17));
        assert_eq!(oldest.added_bytes, Some(3345));
        assert_eq!(oldest.time.to_string(), "2026-01-01T13:00:00Z");
    }

    #[test]
    fn the_short_id_is_the_first_8_characters_of_the_id() {
        let views = parse_snapshots(FIXTURE).unwrap();

        for view in &views {
            assert_eq!(view.short_id.len(), 8);
            assert!(view.id.starts_with(&view.short_id));
        }
    }

    #[test]
    fn a_snapshot_without_a_summary_has_no_counts() {
        let json = br#"[{"time":"2026-01-01T10:00:00-03:00","paths":["/a"],"hostname":"h","id":"0123456789abcdef"}]"#;

        let views = parse_snapshots(json).unwrap();

        assert_eq!(views[0].files, None);
        assert_eq!(views[0].bytes, None);
        assert_eq!(views[0].added_bytes, None);
        assert_eq!(views[0].short_id, "01234567");
    }

    #[test]
    fn an_empty_repository_has_no_snapshots() {
        assert!(parse_snapshots(b"[]").unwrap().is_empty());
    }

    #[test]
    fn text_that_is_not_an_array_is_an_error() {
        assert!(parse_snapshots(b"nope").is_err());
        assert!(parse_snapshots(br#"{"id":"x"}"#).is_err());
    }
}
