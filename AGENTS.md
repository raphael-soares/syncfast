# AGENTS.md

## Layout
- `crates/syncfast-core`: library with all logic (paths, config, filters, bisync, state, units, rclone, network, notifications). No egui, eframe, winit or fastframe UI crates; `tests/boundary.rs` enforces it.
- `crates/syncfast-cli`: binary `syncfast` (`apply`, `run`, `watch`, `preview`, `status`). The systemd units call it.
- `crates/syncfast-gui`: binary `syncfast-gui` (egui + fastframe). Uses the core as a library and never calls the CLI binary, except in dev mode (`SYNCFAST_SYSTEMD=off`).

## Rules
- Flatpak is the only distribution channel (ADR 0006). Code that behaves differently in the sandbox is gated on `FLATPAK_ID` and nothing else.
- `ui/` only draws and emits `Action`. All I/O goes through the backend (`Command`/`Event`).
- No literal colour outside `theme.rs`, except the two tray badge colours.
- Never log a token or the output of `rclone config dump`.
- All UI text is English. Buttons use Title Case and a verb, with `…` when they ask for more input.
- No em or en dashes anywhere (code, comments, docs, commits).
- Comments default to zero; see the `comentarios` skill. Structural decisions go in `docs/adr/`.
- Commits follow Conventional Commits and carry no `Co-Authored-By`; lefthook enforces both.
- Tests assert observable behaviour (see the `testes` skill). Integration tests use only `type = local` rclone remotes and an explicit `Paths`.
- Definition of done: `just check` is green and the new behaviour has a test.

## Environment
- Toolchain is pinned in `rust-toolchain.toml`. Run `just hooks` once per clone.
- Dev mode: `just run-gui-dev` / `just run-cli-dev <args>` use `./.dev/` and `SYNCFAST_SYSTEMD=off`.
- The `[patch.crates-io]` block in `Cargo.toml` pins the crmne egui and winit forks. If it ever stops resolving against the fastframe tag, remove the whole block (fastframe works with upstream egui and winit) and note it here.
