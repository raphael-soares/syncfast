# AGENTS.md

## Layout
- `crates/syncfast-core`: library with all logic (paths, config, filters, bisync, state, units, rclone, restic, backup, network, notifications). No egui, eframe, winit or fastframe UI crates; `tests/boundary.rs` enforces it.
- `crates/syncfast-cli`: binary `syncfast` (`apply`, `run`, `watch`, `preview`, `status`, the agent commands, `mcp`, `agent-guide`). The systemd units call it. Commands and MCP tools only translate to `syncfast_core::ops` (ADR 0007); the agent guide is `docs/agents.md`.
- `crates/syncfast-gui`: binary `syncfast-gui` (egui + fastframe). Uses the core as a library and never calls the CLI binary, except in dev mode (`SYNCFAST_SYSTEMD=off`).

## Rules
- Flatpak is the only distribution channel (ADR 0006). Code that behaves differently in the sandbox is gated on `FLATPAK_ID` and nothing else.
- `ui/` only draws and emits `Action`. All I/O goes through the backend (`Command`/`Event`).
- No literal colour outside `theme.rs`, except the two tray badge colours.
- Never log a token or the output of `rclone config dump`.
- Never log, print or return the backup key.
- All UI text is English. Buttons use Title Case and a verb, with `…` when they ask for more input.
- No em or en dashes anywhere (code, comments, docs, commits).
- Comments default to zero; see the `comentarios` skill. Structural decisions go in `docs/adr/`.
- Commits follow Conventional Commits and carry no `Co-Authored-By`; lefthook enforces both.
- Tests assert observable behaviour (see the `testes` skill). Integration tests use only `type = local` rclone remotes and an explicit `Paths`.
- Definition of done: `just check` is green and the new behaviour has a test.

## Docs
- `PRODUCT.md` (scope and principles), `CONTRIBUTING.md`, `docs/guide/` (user guides), `docs/agents.md` (the agent guide, embedded in the CLI with `include_str!`), `docs/adr/`, `docs/design/` (phase 1 spec and UI mock). README images are in `assets/`.

## Environment
- Toolchain is pinned in `rust-toolchain.toml`. Run `just hooks` once per clone.
- Dev mode: `just run-gui-dev` / `just run-cli-dev <args>` use `./.dev/` and `SYNCFAST_SYSTEMD=off`.
- The `[patch.crates-io]` block in `Cargo.toml` pins the crmne egui and winit forks. If it ever stops resolving against the fastframe tag, remove the whole block (fastframe works with upstream egui and winit) and note it here.
