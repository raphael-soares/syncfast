# 0004: fastframe visuals with Apple HIG structure

## Context
The user runs other apps from the same family (Spotifast, ZapFast) and wants this one to look and theme like them. Window composition and behaviour still need a coherent convention.

## Decision
- Visuals come from the fastframe crates: the 16-token palette, Inter, Lucide icons, user themes in `$XDG_CONFIG_HOME/syncfast/themes/`, a Noctalia template, a violet accent.
- Structure and behaviour follow the Apple HIG (macOS): split view with a sidebar, toolbar in the detail, sheets for modal tasks (one at a time, always with Cancel), alerts only for rare destructive actions, a separate Settings window, a tray menu with constant items (disable instead of hide), determinate progress when measurable, errors next to the problem, verb buttons in Title Case.
- The UI vocabulary says "sync" for a pair of folders; the code says `pair`.
- English only, no i18n.
- The approved mock is `docs/design/ui-mock.html`.

## Consequences
- Screens stay testable through the accessibility tree with `egui_kittest`, without pixel snapshots.
- Choices that look macOS-specific on Linux are intentional and documented here.
