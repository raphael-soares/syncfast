# Contributing to syncfast

Read [PRODUCT.md](PRODUCT.md) first: it says what syncfast will and will not do. For a bug, use the bug form. For a feature, open an issue that states the problem before writing code.

## Setup

Tools are pinned in `mise.toml` (`just`, `cargo-nextest`, `lefthook`), the Rust toolchain in `rust-toolchain.toml`.

```sh
mise install
just hooks
just check
```

Without mise, install `just`, `lefthook`, `cargo-deny` and `cargo-nextest` any other way. On Arch: `sudo pacman -S --needed just lefthook cargo-deny`, then `cargo install --locked cargo-nextest cargo-mutants`.

If the build reports a missing native library, install the one named in the error. Likely candidates: `libxkbcommon wayland libx11 libxcursor libxrandr libxi mesa fontconfig dbus`.

`just run-gui-dev` and `just run-cli-dev <args>` run against an isolated `./.dev/` tree with an rclone remote (`devremote`, an alias of `./.dev/remote`) and no systemd. `just dev-reset` wipes it.

The rclone log fixtures in `crates/syncfast-core/tests/fixtures/` come from the installed rclone. Run `just capture-fixtures` after an rclone upgrade.

To build the Flatpak, install `org.flatpak.Builder` (`flatpak install --user flathub org.flatpak.Builder`), then `just flatpak` builds and installs it for this user and `just flatpak-bundle` also writes `syncfast.flatpak`. After a `Cargo.lock` change, run `just flatpak-sources`.

## Checks

`just check` runs what CI runs, plus doc tests and `cargo deny`:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --workspace
```

New behavior comes with a test that asserts what a user or caller observes. [AGENTS.md](AGENTS.md) has the architecture and the code rules (no em or en dashes, English UI text, comments only when they carry something non-obvious).

## Decisions

Structural decisions live in [docs/adr/](docs/adr/), one short file each (context, decision, consequences). Add or amend an ADR in the same PR as the change. The original design is in [docs/design/](docs/design/).

## Commits and pull requests

Commits follow [Conventional Commits](https://www.conventionalcommits.org/): `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `build`, `chore`, with an optional scope. lefthook rejects anything else and any `Co-Authored-By` trailer.

Keep each PR to one change. `main` has a linear history and receives squash merges, so the PR title must be a valid commit subject.

## Releases

Do not edit versions or `CHANGELOG.md` by hand. On every push to `main`, release-please opens or updates the PR `chore(main): release X.Y.Z` from the commit types: `feat` bumps the minor version, `fix` the patch, and `!` or `BREAKING CHANGE:` bumps the minor while the version is 0.x. `test`, `build` and `chore` open no PR. The release workflow squash-merges that PR itself, tags `vX.Y.Z`, creates the GitHub Release, then builds and signs the Flatpak, publishes it to Pages and attaches the bundle. `main` between releases never reaches users.

By contributing, you agree that your contribution is licensed under the project's MIT License.
