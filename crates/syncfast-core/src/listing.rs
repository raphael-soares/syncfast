//! The listings `rclone bisync` keeps in its workdir after each good sync
//! (`<base>.path1.lst`, `<base>.path2.lst`): the files each side had then.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

const HEADER: &str = "# bisync listing v1";
const FILE_TYPE: &str = "-";

/// The files each side had at the last good sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastSync {
    pub local: HashSet<String>,
    pub remote: HashSet<String>,
}

/// Reads the newest listing pair in `workdir`. A listing rclone set aside
/// as `.lst-err` after a critical error still describes the last good
/// sync, so it is used when the `.lst` is gone. `None` when there is none.
#[must_use]
pub fn last_sync(workdir: &Path) -> Option<LastSync> {
    let base = newest_base(workdir)?;
    let read = |side: &str| {
        ["lst", "lst-err"].iter().find_map(|ext| {
            let text =
                std::fs::read_to_string(workdir.join(format!("{base}.{side}.{ext}"))).ok()?;
            parse_files(&text)
        })
    };
    Some(LastSync {
        local: read("path1").unwrap_or_default(),
        remote: read("path2").unwrap_or_default(),
    })
}

fn newest_base(workdir: &Path) -> Option<String> {
    std::fs::read_dir(workdir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let base = name
                .strip_suffix(".path1.lst")
                .or_else(|| name.strip_suffix(".path1.lst-err"))?
                .to_owned();
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((modified, base))
        })
        .max()
        .map(|(_, base)| base)
}

/// The file paths of one listing; folders are left out. `None` when the
/// text is not a bisync listing.
#[must_use]
pub fn parse_files(text: &str) -> Option<HashSet<String>> {
    let mut lines = text.lines();
    if !lines.next()?.starts_with(HEADER) {
        return None;
    }
    Some(
        lines
            .filter(|line| line.split_whitespace().next() == Some(FILE_TYPE))
            .filter_map(|line| unquote(&line[line.find('"')?..]))
            .collect(),
    )
}

/// Reverses Go's `%q`, which rclone uses for paths in listings and log
/// notices: printable Unicode stays as is, the rest is escaped.
pub(crate) fn unquote(quoted: &str) -> Option<String> {
    let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        let decoded = if c == '\\' {
            match chars.next()? {
                'x' => {
                    bytes.push(u8::try_from(hex(&mut chars, 2)?).ok()?);
                    continue;
                }
                'u' => char::from_u32(hex(&mut chars, 4)?)?,
                'U' => char::from_u32(hex(&mut chars, 8)?)?,
                'a' => '\u{7}',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'v' => '\u{b}',
                other @ ('\\' | '"' | '\'') => other,
                _ => return None,
            }
        } else {
            c
        };
        let mut buf = [0; 4];
        bytes.extend_from_slice(decoded.encode_utf8(&mut buf).as_bytes());
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn hex(chars: &mut std::str::Chars<'_>, digits: usize) -> Option<u32> {
    let text: String = chars.take(digits).collect();
    if text.len() != digits {
        return None;
    }
    u32::from_str_radix(&text, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"# bisync listing v1 from 2026-11-07T16:54:31.809000000+0000
d       -1 - - 2026-11-07T16:54:31.809000000+0000 "0-Archive"
-      12 md5:0ccc - 2026-11-07T16:54:31.809000000+0000 "0-Archive/plain.txt"
-       7 - - 2026-11-07T16:54:31.809000000+0000 "[ÉTICA] Escopo_Apresentação.docx"
-       3 - - 2026-11-07T16:54:31.809000000+0000 "say \"hi\"\\back.txt"
-       3 - - 2026-11-07T16:54:31.809000000+0000 "tab\there\x7f.txt"
"#;

    #[test]
    fn parse_files_keeps_files_unquoted_and_leaves_folders_out() {
        let files = parse_files(LISTING).unwrap();

        assert_eq!(
            files,
            HashSet::from([
                "0-Archive/plain.txt".to_owned(),
                "[ÉTICA] Escopo_Apresentação.docx".to_owned(),
                "say \"hi\"\\back.txt".to_owned(),
                "tab\there\u{7f}.txt".to_owned(),
            ])
        );
    }

    #[test]
    fn parse_files_rejects_text_without_the_listing_header() {
        assert_eq!(parse_files("- 1 - - x \"a\"\n"), None);
    }

    #[test]
    fn unquote_reads_unicode_escapes() {
        assert_eq!(
            unquote(r#""\u00e9\U0001f600""#).as_deref(),
            Some("é\u{1f600}")
        );
        assert_eq!(unquote(r#""broken\q""#), None);
        assert_eq!(unquote("no quotes"), None);
    }

    #[test]
    fn last_sync_reads_both_sides_and_falls_back_to_the_set_aside_listing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a..b.path1.lst"), LISTING).unwrap();
        std::fs::write(
            dir.path().join("a..b.path2.lst-err"),
            "# bisync listing v1 from x\n- 1 - - x \"remote.txt\"\n",
        )
        .unwrap();

        let last = last_sync(dir.path()).unwrap();

        assert!(last.local.contains("0-Archive/plain.txt"));
        assert_eq!(last.remote, HashSet::from(["remote.txt".to_owned()]));
    }

    #[test]
    fn last_sync_of_a_workdir_without_listings_is_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(last_sync(dir.path()), None);
        assert_eq!(last_sync(&dir.path().join("missing")), None);
    }
}
