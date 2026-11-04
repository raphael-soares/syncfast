//! The I/O behind the Conflicts and Filters tabs and `Remove Sync…`: what the
//! backend runs when those screens ask. Errors are the text the screen shows.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jiff::Zoned;
use syncfast_core::config::{Config, Pair};
use syncfast_core::conflicts::{self, Conflict, Resolution};
use syncfast_core::filters::{self, DirMatcher, ExclusionSummary};
use syncfast_core::ops;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;

static TEST_FILES: AtomicU64 = AtomicU64::new(0);

async fn find_pair(paths: &Paths, id: &str) -> Result<Pair, String> {
    let config = Config::load(paths).await.map_err(|err| err.to_string())?;
    config
        .pairs()
        .iter()
        .find(|pair| pair.id == id)
        .cloned()
        .ok_or_else(|| format!("there is no sync with id \"{id}\""))
}

/// The conflicts in the sync's local folder. A folder that is not there has
/// none.
///
/// # Errors
/// The config cannot be read, there is no such sync, or the folder cannot
/// be listed.
pub async fn scan_conflicts(paths: &Paths, id: &str) -> Result<Vec<Conflict>, String> {
    let pair = find_pair(paths, id).await?;
    let matcher = DirMatcher::new(&filters::render_live(&pair).await.text);
    let local = pair.local;
    let scanned = tokio::task::spawn_blocking(move || {
        conflicts::list_conflicts(&local, &matcher).map_err(|err| (local, err))
    })
    .await
    .map_err(|err| err.to_string())?;
    match scanned {
        Ok(found) => Ok(found),
        Err((_, err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err((local, err)) => Err(format!(
            "cannot look for conflicts in {}: {err}",
            local.display()
        )),
    }
}

/// Settles one conflict of the sync, a path relative to its folder.
///
/// # Errors
/// The config cannot be read, there is no such sync, or a file cannot be
/// moved or deleted.
pub async fn resolve_conflict(
    paths: &Paths,
    id: &str,
    conflict: PathBuf,
    resolution: Resolution,
) -> Result<(), String> {
    let pair = find_pair(paths, id).await?;
    let versions = paths.versions_dir(id);
    tokio::task::spawn_blocking(move || {
        conflicts::resolve(&pair.local, &conflict, resolution, &versions, &Zoned::now())
            .map_err(|err| format!("cannot settle {}: {err}", conflict.display()))
    })
    .await
    .map_err(|err| err.to_string())?
}

/// Lists the files of `local` with and without the rules and says what they
/// leave out.
///
/// # Errors
/// rclone is missing or cannot list the folder, or the rules cannot be
/// written for it.
pub async fn test_filters(
    paths: &Paths,
    local: &Path,
    presets: &[String],
    rules: &[String],
) -> Result<ExclusionSummary, String> {
    let folder = local
        .to_str()
        .ok_or_else(|| "the folder's path is not valid text".to_owned())?;
    let filters::Rendered {
        text: rendered,
        git_repos,
    } = filters::render_live(&Pair {
        id: "test".to_owned(),
        name: "test".to_owned(),
        local: local.to_owned(),
        remote: "test:".to_owned(),
        enabled: true,
        presets: presets.to_vec(),
        filters: rules.to_vec(),
    })
    .await;
    let dir = paths.state_dir().join("filters");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("cannot write the test filters: {err}"))?;
    let file = dir.join(format!(
        "test-{}-{}.txt",
        std::process::id(),
        TEST_FILES.fetch_add(1, Ordering::Relaxed)
    ));
    tokio::fs::write(&file, rendered)
        .await
        .map_err(|err| format!("cannot write the test filters: {err}"))?;
    let listed = list_both(folder, &file).await;
    if let Err(err) = tokio::fs::remove_file(&file).await {
        log::warn!("cannot delete {}: {err}", file.display());
    }
    let (all, kept) = listed?;
    Ok(filters::summarize_exclusions(&all, &kept, git_repos))
}

async fn list_both(folder: &str, filters: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let rclone = Rclone::from_env().map_err(|err| err.to_string())?;
    let all = rclone
        .list_files(folder, None)
        .await
        .map_err(|err| err.to_string())?;
    let kept = rclone
        .list_files(folder, Some(filters))
        .await
        .map_err(|err| err.to_string())?;
    Ok((all, kept))
}

/// Writes the presets and custom rules of the sync into the config.
///
/// # Errors
/// The config cannot be read or written, or there is no such sync.
pub async fn write_filters(
    paths: &Paths,
    id: &str,
    presets: &[String],
    rules: &[String],
) -> Result<(), String> {
    let mut config = Config::load(paths).await.map_err(|err| err.to_string())?;
    if !config.set_filters(id, presets, rules) {
        return Err(format!("there is no sync with id \"{id}\""));
    }
    config.save(paths).await.map_err(|err| err.to_string())
}

/// After the filters changed: see [`ops::after_filters_saved`]. Returns whether
/// the sync had no gate at all.
///
/// # Errors
/// The state of the sync cannot be read or written.
pub async fn after_filters_saved(paths: &Paths, id: &str) -> Result<bool, String> {
    ops::after_filters_saved(paths, id)
        .await
        .map_err(|err| err.to_string())
}

/// Takes the sync out of the config. Its units go with the next `apply`.
///
/// # Errors
/// The config cannot be read or written, or there is no such sync.
pub async fn remove_from_config(paths: &Paths, id: &str) -> Result<(), String> {
    let mut config = Config::load(paths).await.map_err(|err| err.to_string())?;
    if !config.remove_pair(id) {
        return Err(format!("there is no sync with id \"{id}\""));
    }
    config.save(paths).await.map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use syncfast_core::state::{Gate, GateKind, State};

    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths::new(
            dir.join("home"),
            dir.join("config"),
            dir.join("state"),
            dir.join("data"),
            dir.join("run"),
        )
    }

    fn write_config(paths: &Paths, local: &Path) {
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            file,
            format!(
                "version = 1\n\n[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"{}\"\nremote = \"r:notes\"\n",
                local.display()
            ),
        )
        .unwrap();
    }

    fn set_gate(paths: &Paths, kind: Option<GateKind>) {
        let state = State {
            gate: kind.map(|kind| Gate {
                kind,
                since: Timestamp::UNIX_EPOCH,
                detail: String::new(),
            }),
            ..State::default()
        };
        let file = paths.state_file("notes");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, serde_json::to_vec(&state).unwrap()).unwrap();
    }

    fn gate_of(paths: &Paths) -> Option<GateKind> {
        let text = std::fs::read_to_string(paths.state_file("notes")).unwrap();
        serde_json::from_str::<State>(&text)
            .unwrap()
            .gate
            .map(|gate| gate.kind)
    }

    #[tokio::test]
    async fn scan_finds_conflicts_outside_the_directories_the_filters_exclude() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("notes");
        std::fs::create_dir_all(local.join("node_modules")).unwrap();
        std::fs::write(local.join("a.md.conflict1"), "x").unwrap();
        std::fs::write(local.join("node_modules/b.js.conflict1"), "x").unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &local);

        let found = scan_conflicts(&paths, "notes").await.unwrap();

        let listed: Vec<_> = found.iter().map(|conflict| conflict.path.clone()).collect();
        assert_eq!(listed, vec![PathBuf::from("a.md.conflict1")]);
    }

    #[tokio::test]
    async fn scan_of_a_missing_folder_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &dir.path().join("gone"));

        assert!(scan_conflicts(&paths, "notes").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn scan_of_an_unknown_sync_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, dir.path());

        let err = scan_conflicts(&paths, "other").await.unwrap_err();

        assert_eq!(err, "there is no sync with id \"other\"");
    }

    #[tokio::test]
    async fn keeping_the_conflicted_copy_files_the_current_one_under_the_versions_of_the_sync() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("notes");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("a.md"), "current").unwrap();
        std::fs::write(local.join("a.md.conflict1"), "old").unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &local);

        resolve_conflict(
            &paths,
            "notes",
            PathBuf::from("a.md.conflict1"),
            Resolution::KeepConflicted,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(local.join("a.md")).unwrap(), "old");
        let stamped = std::fs::read_dir(paths.versions_dir("notes"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read_to_string(stamped.join("a.md")).unwrap(),
            "current"
        );
    }

    #[tokio::test]
    async fn saving_filters_writes_presets_and_rules_to_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, dir.path());

        write_filters(
            &paths,
            "notes",
            &["dev_dirs".to_owned()],
            &["- *.bak".to_owned()],
        )
        .await
        .unwrap();

        let pair = find_pair(&paths, "notes").await.unwrap();
        assert_eq!(pair.presets, ["dev_dirs"]);
        assert_eq!(pair.filters, ["- *.bak"]);
    }

    #[tokio::test]
    async fn new_filters_turn_a_force_gate_into_a_review_of_the_filters() {
        for held in [GateKind::TooManyDeletes, GateKind::AllFilesChanged] {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths(dir.path());
            set_gate(&paths, Some(held));

            let was_clear = after_filters_saved(&paths, "notes").await.unwrap();

            assert!(!was_clear);
            assert_eq!(gate_of(&paths), Some(GateKind::NeedsReviewFiltersChanged));
        }
    }

    #[tokio::test]
    async fn new_filters_leave_other_gates_alone_and_report_a_clear_sync() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        set_gate(&paths, Some(GateKind::AuthError));

        assert!(!after_filters_saved(&paths, "notes").await.unwrap());
        assert_eq!(gate_of(&paths), Some(GateKind::AuthError));

        set_gate(&paths, None);

        assert!(after_filters_saved(&paths, "notes").await.unwrap());
        assert_eq!(gate_of(&paths), None);
    }

    #[tokio::test]
    async fn removing_a_sync_takes_it_out_of_the_config_and_an_unknown_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, dir.path());

        remove_from_config(&paths, "notes").await.unwrap();

        assert!(Config::load(&paths).await.unwrap().pairs().is_empty());
        assert!(remove_from_config(&paths, "notes").await.is_err());
    }

    #[tokio::test]
    async fn test_filters_counts_the_files_the_rules_leave_out_by_top_folder() {
        if Rclone::from_env().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("notes");
        for file in [
            "a.txt",
            "b.log",
            "node_modules/x.js",
            "src/c.rs",
            "src/d.log",
        ] {
            let path = local.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let paths = paths(dir.path());

        let summary = test_filters(
            &paths,
            &local,
            &["system_junk".to_owned(), "dev_dirs".to_owned()],
            &["- *.rs".to_owned()],
        )
        .await
        .unwrap();

        assert_eq!(summary.total, 5);
        assert_eq!(summary.excluded, 4);
        assert_eq!(
            summary.folders,
            vec![("src".to_owned(), 2), ("node_modules".to_owned(), 1)]
        );
        let left_over = std::fs::read_dir(paths.state_dir().join("filters"))
            .unwrap()
            .count();
        assert_eq!(left_over, 0);
    }

    #[tokio::test]
    async fn test_filters_of_a_missing_folder_says_why() {
        if Rclone::from_env().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();

        let err = test_filters(&paths(dir.path()), &dir.path().join("gone"), &[], &[])
            .await
            .unwrap_err();

        assert!(err.contains("directory not found"), "{err}");
    }
}
