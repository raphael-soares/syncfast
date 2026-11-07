#!/usr/bin/env bash
# Captures how `rclone bisync` treats Google Docs, for the phase 2 spike.
# Usage: capture_gdocs.sh <remote> <folder>
# Runs against a REAL Google Drive: it creates <remote>:<folder> and purges it
# at the end. It refuses to start when that folder already holds anything.
# Never part of `just check`; the automated tests use `type = local` remotes.
# Needs rclone and python3 on PATH. Local files live in a tempdir.
set -euo pipefail

if [[ $# -ne 2 || -z "$1" || -z "$2" || "$2" == */* ]]; then
  echo "usage: $0 <remote> <folder>   (folder is a single name at the remote root)" >&2
  exit 1
fi
remote="${1%:}"
folder="$2"
base="$remote:$folder"

if [[ "$(id -u)" -eq 0 ]]; then
  echo "refusing to run as root" >&2
  exit 1
fi

if existing="$(rclone lsf "$base" 2>/dev/null)" && [[ -n "$existing" ]]; then
  echo "refusing to run: $base already has content" >&2
  exit 1
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
version="$(rclone version | head -n 1 | sed -E 's/^rclone v?//')"
out="$here/fixtures/rclone-$version/gdocs"
rm -rf "$out"
mkdir -p "$out"

root="$(mktemp -d)"
cleanup() {
  rm -rf "$root"
  rclone purge --drive-use-trash=false "$base" >/dev/null 2>&1 || true
}
trap cleanup EXIT

export_flags=(--drive-export-formats docx,xlsx,pptx,svg)
url_flags=(--drive-export-formats url)

office() { # office docx|xlsx <file> <text>
  python3 - "$@" <<'PY'
import sys
import zipfile

kind, path, text = sys.argv[1:4]
xml = '<?xml version="1.0" encoding="UTF-8"?>'
ct = "http://schemas.openxmlformats.org/package/2006/content-types"
rel = "http://schemas.openxmlformats.org/package/2006/relationships"
doc = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
    if kind == "docx":
        main = "word/document.xml"
        z.writestr("[Content_Types].xml", f'{xml}<Types xmlns="{ct}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/{main}" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
        z.writestr("_rels/.rels", f'{xml}<Relationships xmlns="{rel}"><Relationship Id="rId1" Type="{doc}/officeDocument" Target="{main}"/></Relationships>')
        z.writestr(main, f'{xml}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>')
    else:
        main = "xl/workbook.xml"
        z.writestr("[Content_Types].xml", f'{xml}<Types xmlns="{ct}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/{main}" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>')
        z.writestr("_rels/.rels", f'{xml}<Relationships xmlns="{rel}"><Relationship Id="rId1" Type="{doc}/officeDocument" Target="{main}"/></Relationships>')
        z.writestr(main, f'{xml}<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{doc}"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>')
        z.writestr("xl/_rels/workbook.xml.rels", f'{xml}<Relationships xmlns="{rel}"><Relationship Id="rId1" Type="{doc}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>')
        z.writestr("xl/worksheets/sheet1.xml", f'{xml}<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>{text}</t></is></c></row></sheetData></worksheet>')
PY
}

# Per scenario: fresh local/ state/ seed/ and a remote subfolder of its own.
setup() {
  rm -rf "$root/local" "$root/state" "$root/seed" "$root/backup"
  mkdir -p "$root/local" "$root/state" "$root/seed"
  remote_dir="$base/$1"
}

seed_docs() {
  office docx "$root/seed/notes.docx" "notes v1"
  office docx "$root/seed/plan.docx" "plan v1"
  office xlsx "$root/seed/budget.xlsx" "budget v1"
  printf 'plain v1\n' > "$root/seed/plain.txt"
  rclone copy --drive-import-formats docx,xlsx "$root/seed" "$remote_dir"
  local docs
  docs="$(rclone lsjson "$remote_dir" | grep -c '"Size":-1' || true)"
  if [[ "$docs" -ne 3 ]]; then
    echo "import did not create 3 Google Docs in $remote_dir (found $docs)" >&2
    exit 1
  fi
}

# Flags are the ones the app sends minus --drive-skip-gdocs, --max-delete and
# --backup-dir1, which the scenarios that study them add themselves.
bisync() {
  local log="$1"
  shift
  rclone bisync "$root/local" "$remote_dir" \
    --workdir "$root/state/" \
    --resilient --recover --max-lock 2m \
    --conflict-resolve newer --conflict-loser num \
    --create-empty-src-dirs \
    --stats 2s --stats-log-level NOTICE \
    --use-json-log --log-file "$log" -v \
    "$@"
}

prepare() {
  local attempt
  for attempt in 1 2 3; do
    if bisync "$root/prepare.jsonl" --resync --resync-mode newer "$@"; then
      return 0
    fi
    echo "prepare failed (attempt $attempt):" >&2
    grep '"level":"error"' "$root/prepare.jsonl" | tail -n 5 >&2 || true
    rm -rf "$root/local" "$root/state"
    mkdir -p "$root/local" "$root/state"
    sleep 5
  done
  exit 1
}

# Listing flags: the subset of a bisync flag list that changes what rclone shows.
list=()
pick_list_flags() {
  list=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --drive-export-formats) list+=("$1" "$2"); shift 2 ;;
      --drive-skip-gdocs) list+=("$1"); shift ;;
      *) shift ;;
    esac
  done
}

record() { # record <name> <exit> <flags...>
  local name="$1" code="$2"
  shift 2
  echo "$code" > "$out/$name.exit"
  printf '%s\n' "$*" > "$out/$name.flags"
  pick_list_flags "$@"
  rclone lsjson -R "$root/local" > "$out/$name.local.json"
  rclone lsjson -R "${list[@]}" "$remote_dir" > "$out/$name.remote.json"
  rclone lsjson -R --drive-trashed-only "${list[@]}" "$remote_dir" > "$out/$name.trash.json"
  if [[ -d "$root/backup" ]]; then
    (cd "$root/backup" && find . -type f | sort) > "$out/$name.backup.txt"
  fi
  echo "$name: exit $code"
}

capture() { # capture <name> <flags...>
  local name="$1" code=0
  shift
  rm -f "$out/$name".*
  pick_list_flags "$@"
  rclone lsjson -R "$root/local" > "$out/$name.local_before.json"
  rclone lsjson -R "${list[@]}" "$remote_dir" > "$out/$name.remote_before.json"
  bisync "$out/$name.jsonl" "$@" || code=$?
  record "$name" "$code" "$@"
}

# again: the same flags once more, to see whether the result is stable.
again() { # again <name> <flags...>
  local name="$1"
  shift
  capture "${name}_again" "$@"
}

remote_edit() { # remote_edit <file> <text>: new content for an existing Doc
  office docx "$root/edit.docx" "$2"
  rclone copyto --drive-import-formats docx "$root/edit.docx" "$remote_dir/$1"
}

# first_sync: what a first run brings to disk, dry run and real.
first_sync() { # first_sync <prefix> <flags...>
  local prefix="$1"
  shift
  setup "${prefix}_first_sync"
  seed_docs
  capture "${prefix}_first_sync_dry_run" --resync --resync-mode newer --dry-run "$@"
  capture "${prefix}_first_sync" --resync --resync-mode newer "$@"
}

# edit_local: a local edit of an exported file, with and without import.
edit_local() { # edit_local <name> <file> <kind> <import formats|-> <flags...>
  local name="$1" file="$2" kind="$3" imp="$4"
  shift 4
  local extra=()
  [[ "$imp" != "-" ]] && extra=(--drive-import-formats "$imp")
  setup "$name"
  seed_docs
  prepare "$@" "${extra[@]}"
  office "$kind" "$root/local/$file" "$file edited locally"
  capture "$name" "$@" "${extra[@]}"
  if [[ "$(cat "$out/$name.exit")" -eq 0 ]]; then
    again "$name" "$@" "${extra[@]}"
  else
    capture "${name}_recover" --resync --resync-mode newer "$@" "${extra[@]}"
  fi
}

edit_local_url() {
  setup url_edit_local
  seed_docs
  prepare "${url_flags[@]}"
  printf '[InternetShortcut]\nURL=https://example.invalid/edited\n' > "$root/local/notes.url"
  capture url_edit_local "${url_flags[@]}"
  capture url_edit_local_recover --resync --resync-mode newer "${url_flags[@]}"
}

edit_remote() { # edit_remote <prefix> <flags...>
  local prefix="$1"
  shift
  setup "${prefix}_edit_remote"
  seed_docs
  prepare "$@"
  sleep 2
  remote_edit notes.docx "notes edited in Drive"
  capture "${prefix}_edit_remote" "$@"
  again "${prefix}_edit_remote" "$@"
}

edit_both() {
  setup export_edit_both
  seed_docs
  prepare "${export_flags[@]}" --drive-import-formats docx
  sleep 2
  remote_edit notes.docx "notes edited in Drive"
  office docx "$root/local/notes.docx" "notes edited locally"
  capture export_edit_both "${export_flags[@]}" --drive-import-formats docx
  again export_edit_both "${export_flags[@]}" --drive-import-formats docx
}

edit_both_no_import() {
  setup export_edit_both_no_import
  seed_docs
  prepare "${export_flags[@]}"
  sleep 2
  remote_edit notes.docx "notes edited in Drive"
  office docx "$root/local/notes.docx" "notes edited locally"
  capture export_edit_both_no_import "${export_flags[@]}"
}

no_changes() { # no_changes <prefix> <flags...>
  local prefix="$1"
  shift
  setup "${prefix}_no_changes"
  seed_docs
  prepare "$@"
  capture "${prefix}_no_changes" "$@"
}

# new_local: a docx created on disk that no Doc corresponds to.
new_local() { # new_local <name> <import formats|-> <flags...>
  local name="$1" imp="$2"
  shift 2
  local extra=()
  [[ "$imp" != "-" ]] && extra=(--drive-import-formats "$imp")
  setup "$name"
  seed_docs
  prepare "$@" "${extra[@]}"
  office docx "$root/local/fresh.docx" "created on disk"
  capture "$name" "$@" "${extra[@]}"
  again "$name" "$@" "${extra[@]}"
}

delete_local() { # delete_local <prefix> <file> <flags...>
  local prefix="$1" file="$2"
  shift 2
  setup "${prefix}_delete_local"
  seed_docs
  prepare "$@"
  rm "$root/local/$file"
  capture "${prefix}_delete_local" "$@"
}

rename_remote() { # rename_remote <prefix> <flags...>: budget Sheet renamed in Drive
  local prefix="$1"
  shift
  setup "${prefix}_rename_remote"
  seed_docs
  prepare "$@"
  rclone moveto "$remote_dir/budget.xlsx" "$remote_dir/budget-renamed.xlsx"
  capture "${prefix}_rename_remote" "$@"
  again "${prefix}_rename_remote" "$@"
}

# over_cap: three of four files vanish locally; the cap is 50 percent.
over_cap() { # over_cap <prefix> <doc1> <doc2> <doc3> <flags...>
  local prefix="$1" a="$2" b="$3" c="$4"
  shift 4
  setup "${prefix}_with_max_delete_and_backup_dir1_over_cap"
  seed_docs
  prepare "$@"
  rm "$root/local/$a" "$root/local/$b" "$root/local/$c"
  capture "${prefix}_with_max_delete_and_backup_dir1_over_cap" \
    --max-delete 50 --backup-dir1 "$root/backup/" "$@"
}

# under_cap: one Doc is deleted in Drive, so the local copy is the one lost
# and --backup-dir1 should keep it.
under_cap() { # under_cap <prefix> <flags...>
  local prefix="$1"
  shift
  setup "${prefix}_with_max_delete_and_backup_dir1_under_cap"
  seed_docs
  prepare "$@"
  rclone deletefile "$remote_dir/notes.docx"
  capture "${prefix}_with_max_delete_and_backup_dir1_under_cap" \
    --max-delete 50 --backup-dir1 "$root/backup/" "$@"
}

skip_baseline() {
  setup skip_baseline
  seed_docs
  prepare --drive-skip-gdocs
  printf 'new local file\n' > "$root/local/new.txt"
  capture skip_baseline --drive-skip-gdocs
}

first_sync export "${export_flags[@]}"
first_sync url "${url_flags[@]}"

edit_local export_edit_local_no_import notes.docx docx - "${export_flags[@]}"
edit_local export_edit_local_import notes.docx docx docx "${export_flags[@]}"
edit_local export_edit_local_import_xlsx budget.xlsx xlsx docx,xlsx "${export_flags[@]}"
edit_local_url

edit_remote export "${export_flags[@]}"
edit_remote url "${url_flags[@]}"
edit_both
edit_both_no_import

no_changes export "${export_flags[@]}"
no_changes url "${url_flags[@]}"

new_local export_new_local_no_import - "${export_flags[@]}"
new_local export_new_local_import docx "${export_flags[@]}"

delete_local export notes.docx "${export_flags[@]}"
delete_local url notes.url "${url_flags[@]}"

rename_remote export "${export_flags[@]}"
rename_remote url "${url_flags[@]}"

over_cap export notes.docx plan.docx budget.xlsx "${export_flags[@]}"
over_cap url notes.url plan.url budget.url "${url_flags[@]}"
under_cap export "${export_flags[@]}"
under_cap url "${url_flags[@]}"

skip_baseline
