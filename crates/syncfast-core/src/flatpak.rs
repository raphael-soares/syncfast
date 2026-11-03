//! What changes when syncfast runs inside its Flatpak sandbox (ADR 0006).
//! Outside it, [`app_id`] is `None` and nothing here has an effect.

use std::ffi::OsString;

const APP_ID_VAR: &str = "FLATPAK_ID";

/// The sandbox's app id, from `FLATPAK_ID`.
#[must_use]
pub fn app_id() -> Option<String> {
    app_id_from(std::env::var_os(APP_ID_VAR))
}

pub(crate) fn app_id_from(value: Option<OsString>) -> Option<String> {
    value
        .and_then(|id| id.into_string().ok())
        .filter(|id| !id.is_empty())
}

/// The program and arguments that run `systemctl --user <args>`: through
/// `flatpak-spawn --host` when the sandbox has no systemd of its own.
#[must_use]
pub fn systemctl_command(on_host: bool, args: &[&str]) -> (&'static str, Vec<String>) {
    let mut full = Vec::with_capacity(args.len() + 4);
    let program = if on_host {
        full.extend(["--host", "systemctl"].map(str::to_owned));
        "flatpak-spawn"
    } else {
        "systemctl"
    };
    full.push("--user".to_owned());
    full.extend(args.iter().map(|arg| (*arg).to_owned()));
    (program, full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_the_sandbox_systemctl_runs_directly() {
        let (program, args) = systemctl_command(false, &["enable", "--now", "syncfast-a.timer"]);

        assert_eq!(program, "systemctl");
        assert_eq!(args, ["--user", "enable", "--now", "syncfast-a.timer"]);
    }

    #[test]
    fn inside_the_sandbox_systemctl_goes_through_the_host() {
        let (program, args) = systemctl_command(true, &["daemon-reload"]);

        assert_eq!(program, "flatpak-spawn");
        assert_eq!(args, ["--host", "systemctl", "--user", "daemon-reload"]);
    }

    #[test]
    fn an_empty_or_missing_flatpak_id_is_not_a_sandbox() {
        assert_eq!(app_id_from(None), None);
        assert_eq!(app_id_from(Some(OsString::new())), None);
        assert_eq!(
            app_id_from(Some("io.example.App".into())).as_deref(),
            Some("io.example.App")
        );
    }
}
