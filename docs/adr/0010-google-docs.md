# 0010: Google Docs stay skipped

## Context
syncfast passes `--drive-skip-gdocs` to rclone, so Google Docs, Sheets, Slides and Drawings never reach the disk. rclone can instead export them (`--drive-export-formats docx,xlsx,pptx,svg`) or write link files (`--drive-export-formats url`), and import local files back (`--drive-import-formats`). The phase 2 spike ran `rclone bisync` v1.75.2 against a real Drive, in every scenario of the issue, for each way (36 captured runs). The fixtures are in `crates/syncfast-core/tests/fixtures/rclone-1.75.2/gdocs/` and `crates/syncfast-core/tests/capture_gdocs.sh` recaptures them. The full table is in `docs/design/gdocs-spike.md`.

## Decision
- `google_docs` stays `skip`. `GoogleDocs { Skip, Export, Url }` and `--drive-skip-gdocs` are unchanged and no new setting is added.
- `export` is rejected. A change on both sides of an exported Doc sends the Doc to the Drive trash and creates another one with a new ID, or replaces it with a plain file when no import format is set, and bisync replicates the `.conflict1` loser as a plain file on both sides (`export_edit_both`, `export_edit_both_no_import`). Without `--drive-import-formats`, the default, the first local edit of an exported file aborts bisync with a critical error that `--resync` does not clear (`export_edit_local_no_import_recover`). Importing a new local `docx` makes the next run overwrite the user's file with Google's export (`export_new_local_import_again`).
- `url` is not enabled. Its only entry condition holds: deleting a `.url` file is counted as a deletion and the Doc goes to the Drive trash (`url_delete_local`, `url_with_max_delete_and_backup_dir1_over_cap`). But deleting a shortcut deletes the real Doc, a local edit of the `.url` locks the pair (`url_edit_local_recover`), and nothing in the product asks for links to Docs.
- What holds, for a future reader: `--max-delete` and the review count Docs in both modes; a local edit with the matching import format updates the same Doc in place and is stable (`export_edit_local_import_again`); a remote edit or rename is stable (`export_edit_remote_again`, `export_rename_remote_again`); `--backup-dir1` keeps the local copy of a Doc deleted in Drive.

## Consequences
- Google Docs are not part of the sync, as in phase 1.
- Reopening this needs a new spike: a conflict policy for Docs that keeps the Doc identity, and an import format that does not depend on the user setting it.
- An rclone upgrade may change these behaviours. Rerun `capture_gdocs.sh` against a test folder before reconsidering.
