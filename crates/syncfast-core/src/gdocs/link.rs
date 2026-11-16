//! The link file that stands for one native Google file on disk.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::filters::DirMatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub mime: &'static str,
    pub ext: &'static str,
}

pub const KINDS: [Kind; 7] = [
    Kind {
        mime: "application/vnd.google-apps.document",
        ext: "gdoc",
    },
    Kind {
        mime: "application/vnd.google-apps.spreadsheet",
        ext: "gsheet",
    },
    Kind {
        mime: "application/vnd.google-apps.presentation",
        ext: "gslides",
    },
    Kind {
        mime: "application/vnd.google-apps.form",
        ext: "gform",
    },
    Kind {
        mime: "application/vnd.google-apps.drawing",
        ext: "gdraw",
    },
    Kind {
        mime: "application/vnd.google-apps.map",
        ext: "gmap",
    },
    Kind {
        mime: "application/vnd.google-apps.site",
        ext: "gsite",
    },
];

/// A larger file is never read: it is not a link syncfast wrote.
pub const MAX_LINK_BYTES: u64 = 64 * 1024;
const ID_META: &str = "<meta name=\"syncfast-drive-id\" content=\"";
const SLASH_STANDIN: char = '\u{FF0F}';

#[must_use]
pub fn kind_of_mime(mime: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|kind| kind.mime == mime)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

#[must_use]
pub fn render(id: &str, url: &str, title: &str) -> String {
    let (id, url, title) = (escape(id), escape(url), escape(title));
    format!(
        "<!DOCTYPE html>\n\
         <html><head><meta charset=\"utf-8\">\n\
         {ID_META}{id}\">\n\
         <meta http-equiv=\"refresh\" content=\"0; url={url}\">\n\
         <title>{title}</title></head>\n\
         <body><a href=\"{url}\">{title}</a></body></html>\n"
    )
}

/// `text` with the file name it shows blanked, so a link renamed on disk
/// still reads as the one syncfast wrote.
#[must_use]
pub fn untitled(text: &str) -> String {
    let Some(title) = text
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(title, _)| title)
    else {
        return text.to_owned();
    };
    text.replacen(&format!("<title>{title}</title>"), "<title></title>", 1)
        .replacen(&format!("\">{title}</a>"), "\"></a>", 1)
}

/// The Drive file id a link names. Drive ids are letters, digits, `-` and
/// `_`; anything else is not an id syncfast wrote.
#[must_use]
pub fn parse_id(text: &str) -> Option<&str> {
    let start = text.find(ID_META)? + ID_META.len();
    let rest = &text[start..];
    let id = &rest[..rest.find('"')?];
    let valid = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    valid.then_some(id)
}

/// `/` cannot be in a file name; rclone shows it as `／` and so does this.
#[must_use]
pub fn file_name(name: &str, number: Option<u32>, ext: &str) -> String {
    let name = name.replace('/', &SLASH_STANDIN.to_string());
    match number {
        Some(number) => format!("{name} ({number}).{ext}.html"),
        None => format!("{name}.{ext}.html"),
    }
}

#[must_use]
pub fn drive_name(stem: &str) -> String {
    stem.replace(SLASH_STANDIN, "/")
}

/// `Ata.gdoc.html` gives `Ata` and the Docs kind.
#[must_use]
pub fn split_link_name(name: &str) -> Option<(&str, &'static Kind)> {
    let base = name.strip_suffix(".html")?;
    KINDS.iter().find_map(|kind| {
        let stem = base.strip_suffix(kind.ext)?.strip_suffix('.')?;
        (!stem.is_empty()).then_some((stem, kind))
    })
}

/// A link file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLink {
    /// Relative to the sync's folder, with `/` between the parts.
    pub path: String,
    pub id: Option<String>,
    /// `None` when the file is too large or not UTF-8.
    pub text: Option<String>,
}

fn read_small(path: &Path) -> io::Result<Option<String>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_LINK_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_LINK_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LINK_BYTES {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

/// Every link file under `root`, sorted by path. Folders `matcher` excludes
/// are not entered, symlinks are not followed and a name that is not UTF-8
/// is skipped.
///
/// # Errors
/// A folder cannot be read.
pub fn scan(root: &Path, matcher: &DirMatcher) -> io::Result<Vec<LocalLink>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel_dir) = pending.pop() {
        let entries = match std::fs::read_dir(root.join(&rel_dir)) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound && rel_dir.as_os_str().is_empty() => {
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            let rel = rel_dir.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !matcher.is_excluded(&rel) {
                    pending.push(rel);
                }
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !kind.is_file() || split_link_name(&name).is_none() {
                continue;
            }
            let Some(path) = rel.to_str().map(str::to_owned) else {
                continue;
            };
            let text = match read_small(&root.join(&rel)) {
                Ok(text) => text,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let id = text.as_deref().and_then(parse_id).map(str::to_owned);
            found.push(LocalLink { path, id, text });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_comes_back_from_a_rendered_link_with_an_escaped_title() {
        let text = render(
            "1AbC-d_9",
            "https://docs.google.com/document/d/1AbC-d_9/edit",
            "A&B",
        );

        assert_eq!(parse_id(&text), Some("1AbC-d_9"));
        assert!(text.contains("<title>A&amp;B</title>"));
    }

    #[test]
    fn text_without_a_valid_id_names_no_file() {
        assert_eq!(parse_id("<html></html>"), None);
        assert_eq!(
            parse_id("<meta name=\"syncfast-drive-id\" content=\"a b\">"),
            None
        );
        assert_eq!(
            parse_id("<meta name=\"syncfast-drive-id\" content=\"\">"),
            None
        );
    }

    #[test]
    fn a_slash_in_the_drive_name_becomes_the_fullwidth_slash_and_back() {
        let name = file_name("a/b", Some(2), "gdoc");

        assert_eq!(name, "a\u{FF0F}b (2).gdoc.html");
        assert_eq!(drive_name("a\u{FF0F}b"), "a/b");
    }

    #[test]
    fn link_names_need_a_stem_a_known_kind_and_html() {
        assert_eq!(
            split_link_name("Ata.gsheet.html").map(|(stem, kind)| (stem, kind.ext)),
            Some(("Ata", "gsheet"))
        );
        assert!(split_link_name(".gdoc.html").is_none());
        assert!(split_link_name("Ata.gdoc").is_none());
        assert!(split_link_name("Ata.html").is_none());
        assert!(split_link_name("Ata.gdoc.html.conflict1").is_none());
        assert!(split_link_name("Ata.gscript.html").is_none());
    }

    #[test]
    fn scan_finds_links_with_and_without_an_id_and_skips_excluded_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let put = |path: &str, text: &str| {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        put("a/Ata.gdoc.html", &render("id1", "https://x", "Ata"));
        put("b.gsheet.html", "hand made");
        put("c.txt", "plain");
        put("node_modules/x.gdoc.html", &render("id2", "https://x", "x"));

        let found = scan(root, &DirMatcher::new("- node_modules/\n")).unwrap();

        let summary: Vec<(&str, Option<&str>)> = found
            .iter()
            .map(|link| (link.path.as_str(), link.id.as_deref()))
            .collect();
        assert_eq!(
            summary,
            vec![("a/Ata.gdoc.html", Some("id1")), ("b.gsheet.html", None)]
        );
    }

    #[test]
    fn scan_never_reads_a_file_over_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut text = render("id1", "https://x", "Big");
        text.push_str(&" ".repeat(usize::try_from(MAX_LINK_BYTES).unwrap()));
        std::fs::write(dir.path().join("Big.gdoc.html"), text).unwrap();

        let found = scan(dir.path(), &DirMatcher::new("")).unwrap();

        assert_eq!(found[0].id, None);
        assert_eq!(found[0].text, None);
    }
}
