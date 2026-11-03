# 0001: Workspace with core, cli and gui crates

## Context
The same behaviour must be reachable from systemd units (no display), from tests, and from a GUI. Putting logic in the GUI would make it untestable without a window and would force the units to start a UI process.

## Decision
Three crates in a Cargo workspace:
- `syncfast-core` (lib): all logic. It does not depend on egui, eframe, winit or fastframe UI crates; only `fastframe-file`, `fastframe-instance` and `fastframe-notifications` (plus its non-UI dependency `fastframe-desktop`) are allowed. `tests/boundary.rs` fails with the dependency path if that changes.
- `syncfast-cli` (bin `syncfast`): the entry point the units call.
- `syncfast-gui` (bin `syncfast-gui`): links the core as a library. It starts the CLI binary only in dev mode, where systemd is off.

Paths come from one resolver, `Paths::from_env()`, driven by the XDG variables, so tests and dev mode control every location through the environment.

## Consequences
- The core is tested end to end with real rclone on local remotes.
- The GUI is thin: screens are functions of an immutable view model that return actions.
- Adding a crate that touches the UI from the core requires revisiting this ADR.
