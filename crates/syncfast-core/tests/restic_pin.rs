//! The restic version the code expects is the one the Flatpak ships.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use syncfast_core::restic::RESTIC_VERSION;

const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packaging/flatpak/io.github.raphael_soares.syncfast.yml"
);

fn restic_sources() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(MANIFEST).unwrap();
    let module = text
        .split("\n  - name: ")
        .find(|module| module.starts_with("restic\n"))
        .expect("the manifest has a restic module");
    let lines: Vec<&str> = module.lines().map(str::trim).collect();
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let url = line.strip_prefix("url: ")?;
            let sum = lines.get(i + 1)?.strip_prefix("sha256: ")?;
            Some((url.to_owned(), sum.to_owned()))
        })
        .collect()
}

#[test]
fn the_manifest_downloads_the_version_the_constant_names() {
    let sources = restic_sources();

    assert_eq!(sources.len(), 2, "one source per architecture");
    for arch in ["amd64", "arm64"] {
        let expected = format!(
            "https://github.com/restic/restic/releases/download/v{RESTIC_VERSION}/restic_{RESTIC_VERSION}_linux_{arch}.bz2"
        );
        assert!(
            sources.iter().any(|(url, _)| *url == expected),
            "no source for {expected}"
        );
    }
}

#[test]
fn every_source_has_a_full_sha256() {
    for (url, sum) in restic_sources() {
        assert_eq!(sum.len(), 64, "{url}");
        assert!(
            sum.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{url}"
        );
    }
}

#[test]
fn the_two_architectures_have_different_hashes() {
    let sources = restic_sources();

    assert_ne!(sources[0].1, sources[1].1);
}

#[test]
fn the_manifest_reaches_the_secret_service_and_external_disks() {
    let text = std::fs::read_to_string(MANIFEST).unwrap();

    for permission in [
        "--talk-name=org.freedesktop.secrets",
        "--filesystem=/run/media",
        "--filesystem=/mnt",
    ] {
        assert!(text.contains(permission), "{permission}");
    }
}
