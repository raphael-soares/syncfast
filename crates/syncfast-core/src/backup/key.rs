//! The backup key: the password restic encrypts the repository with.
//!
//! It never reaches a log, a state file, argv or the environment of a
//! process (AGENTS.md); restic reads it from a pipe.

use std::fmt;

use data_encoding::BASE32_NOPAD;
use zeroize::Zeroizing;

const KEY_BYTES: usize = 32;
const KEY_CHARS: usize = 52;
const GROUP: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("The key must have 52 letters and digits (A to Z, 2 to 7).")]
    Malformed,
    #[error(
        "Cannot reach the key storage: {0}. Backups need the Secret portal or a Secret Service; see docs/guide/backup.md."
    )]
    Unavailable(String),
    #[error("Cannot create a key: {0}")]
    Random(String),
}

/// 52 characters `A-Z2-7`: 256 bits in RFC 4648 base32 without padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Key(Zeroizing<String>);

impl Key {
    /// # Errors
    /// The system random generator cannot be read.
    pub fn generate() -> Result<Self, KeyError> {
        let mut bytes = Zeroizing::new([0u8; KEY_BYTES]);
        getrandom::fill(&mut bytes[..]).map_err(|err| KeyError::Random(err.to_string()))?;
        Ok(Self(Zeroizing::new(BASE32_NOPAD.encode(&bytes[..]))))
    }

    /// What a person typed or pasted: spaces and `-` are dropped and
    /// lowercase is accepted.
    ///
    /// # Errors
    /// [`KeyError::Malformed`] unless 52 characters of `A-Z2-7` remain.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let normalized: Zeroizing<String> = Zeroizing::new(
            text.chars()
                .filter(|c| !c.is_whitespace() && *c != '-')
                .map(|c| c.to_ascii_uppercase())
                .collect(),
        );
        let valid = normalized.len() == KEY_CHARS
            && normalized
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte));
        if valid {
            Ok(Self(normalized))
        } else {
            Err(KeyError::Malformed)
        }
    }

    /// 13 groups of 4 joined by `-`, for a person to write down. Only the
    /// GUI calls it.
    #[must_use]
    pub fn display_groups(&self) -> String {
        let groups: Vec<&str> = self
            .0
            .as_bytes()
            .chunks(GROUP)
            .filter_map(|chunk| std::str::from_utf8(chunk).ok())
            .collect();
        groups.join("-")
    }

    /// The password restic receives on stdin.
    pub(crate) fn password(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRST";

    #[test]
    fn a_generated_key_is_52_characters_of_the_base32_alphabet() {
        let key = Key::generate().unwrap();

        assert_eq!(key.password().len(), 52);
        assert!(
            key.password()
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte))
        );
    }

    #[test]
    fn two_generated_keys_differ() {
        assert_ne!(Key::generate().unwrap(), Key::generate().unwrap());
    }

    #[test]
    fn a_generated_key_parses_back_from_its_display_form() {
        let key = Key::generate().unwrap();

        assert_eq!(Key::parse(&key.display_groups()).unwrap(), key);
    }

    #[test]
    fn display_groups_are_13_groups_of_4() {
        let shown = Key::parse(SAMPLE).unwrap().display_groups();

        assert_eq!(
            shown,
            "ABCD-EFGH-IJKL-MNOP-QRST-UVWX-YZ23-4567-ABCD-EFGH-IJKL-MNOP-QRST"
        );
        assert_eq!(shown.split('-').count(), 13);
    }

    #[test]
    fn parse_accepts_lowercase_dashes_and_spaces() {
        let messy = format!(
            " {}-{} {}\n",
            SAMPLE[..20].to_lowercase(),
            &SAMPLE[20..40],
            &SAMPLE[40..]
        );

        assert_eq!(Key::parse(&messy).unwrap().password(), SAMPLE);
    }

    #[test]
    fn parse_refuses_the_wrong_length() {
        assert_eq!(Key::parse(&SAMPLE[..51]), Err(KeyError::Malformed));
        assert_eq!(Key::parse(&format!("{SAMPLE}A")), Err(KeyError::Malformed));
        assert_eq!(Key::parse(""), Err(KeyError::Malformed));
    }

    #[test]
    fn parse_refuses_characters_outside_the_alphabet() {
        for bad in ['0', '1', '8', '9', '!', 'é'] {
            let text = format!("{bad}{}", &SAMPLE[1..]);

            assert_eq!(Key::parse(&text), Err(KeyError::Malformed), "{bad}");
        }
    }

    #[test]
    fn the_malformed_message_is_the_one_the_screens_show() {
        assert_eq!(
            KeyError::Malformed.to_string(),
            "The key must have 52 letters and digits (A to Z, 2 to 7)."
        );
    }

    #[test]
    fn debug_never_contains_the_key() {
        let key = Key::parse(SAMPLE).unwrap();

        let shown = format!("{key:?}");

        assert_eq!(shown, "Key(<redacted>)");
        assert!(!shown.contains("ABCD"));
    }

    #[test]
    fn unavailable_names_the_detail_and_the_guide() {
        let message = KeyError::Unavailable("no portal".into()).to_string();

        assert_eq!(
            message,
            "Cannot reach the key storage: no portal. Backups need the Secret portal or a Secret Service; see docs/guide/backup.md."
        );
    }
}
