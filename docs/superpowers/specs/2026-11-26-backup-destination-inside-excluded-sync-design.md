# Backup destination inside a sync that excludes it

## Intent

The owner's `Drive` sync pairs `~/Drive` with the root of the account (`Raphael:`), so every folder of that account is inside a sync. ADR 0009 refuses any backup destination that overlaps a sync, which leaves no place on that account for a backup. Yet the sync already keeps `/Backups` out with its own filter (`- /Backups/**`): rclone never touches that folder. syncfast should accept a destination there.

The owner keeps one `config.toml` per machine in his dotfiles and wants backups ready to turn on later, none defined now. A likely kind of source is another Flatpak's data, for example game worlds under `~/.var/app/org.prismlauncher.PrismLauncher/...`. The syncfast sandbox cannot see that folder even with `--filesystem=home`. I checked: it sees only its own folder under `~/.var/app`, and an explicit `--filesystem=~/.var/app/<id>:ro` makes the other app's folder readable. Today such a folder looks missing or empty, and the user gets no hint why.

## Goals

1. A backup destination that overlaps a sync is accepted when that sync's filters provably exclude every path inside the destination. Applies to account destinations (against the sync's remote) and local ones (against its local folder).
2. That stays true afterwards. A change that would make the sync stop excluding a backup destination is refused, naming the backup. This covers changing its filters, its presets, its folders or its mode. A config edited by hand into that state marks the backup with the same error, through the existing validation.
3. When syncfast runs sandboxed and a backup folder cannot be seen from the sandbox, the error says so and gives the exact command that grants read access.

## Non-goals and anti-goals

- No consistent copies of SQLite or other live files, no pre-backup hooks, no "wait until a program closes".
- No change to the key: no command accepts, prints or returns it (ADR 0007, 0009).
- syncfast never edits a sync's filters to make room for a backup. That would hold the sync for a new review (ADR 0002), a surprise the user did not ask for.
- Anti-goal: any path where bisync could download a restic repository into the local folder or delete its packs. If exclusion cannot be proven, the answer is the refusal of today.

## Rule

Let D be the destination and S a sync it overlaps (same account and one path inside the other, or local folders nested).

- D equal to S's root, or containing it: refused, as today. No filter can help.
- D inside S: take D relative to S's root (remote root for an account destination, local folder for a local one). Accepted when S's rendered filters (presets and custom rules, the same text a round uses) exclude every path below it:
  - a directory rule (`- X/` or `- /X/`) that matches D or a folder above it, or
  - a file rule ending in `/**` (`- /X/**`, `- X/**`) whose prefix matches D or a folder above it.
- Any `+ ` rule in S makes exclusion unprovable (include rules can re-admit anything, the same reason `DirMatcher` turns itself off): refused.
- The versions folder of a `mirror_up` sync stays occupied, as today.

"Matches" means the rule's pattern, under the conventions of `filters.rs` (anchored `/X` from the root, unanchored `X` as `**/X`, `literal_separator`), matches the concrete path of D or of one of its ancestors (taken relative to S's root). Matching a concrete folder proves that rclone excludes that whole folder, so glob rules count: `- Back*/` and `- **/Backups/**` both cover `Backups/minecraft`. A rule that only matches folders *inside* D (`- node_modules/`) proves nothing. (Decided after the builder check.)

Accepting D against S changes nothing else. D still counts as occupied for the other checks: a second backup that overlaps D, and a sync whose root is inside D, are refused as today. The account and key checks are unchanged. When a write to a sync (filters, presets, folders, mode, or a new sync) would leave a backup destination inside it unexcluded, that write is refused and the message names the backup. A config already in that state, for example one edited by hand, shows the error on the backup through `validate_backups`. (Decided after the builder check.)

The owner's own case must pass as a named test: sync `Drive`, remote `Raphael:`, custom rule `- /Backups/**`, destination `Raphael:Backups/<any name>`.

The refusal names the sync and what would fix it, for example: `backup destination is inside "Drive"; exclude it in that sync's filters (- /Backups/**) or pick another folder`.

## Sandbox visibility

Only when running inside the Flatpak (gated on `FLATPAK_ID`, per AGENTS.md). Covers creating a backup, adding folders, and validation. A folder that does not exist inside the sandbox but is under another app's `~/.var/app/<id>`, or outside the home and the sandbox roots (`/run/media`, `/mnt`), gets an error like:

```
"<folder>" is not visible inside the Flatpak sandbox. Grant read access with:
flatpak override --user --filesystem=<folder>:ro io.github.raphael_soares.syncfast
```

The exit code is 2 (a request the user can fix). A folder that is simply missing keeps today's message.

## Approaches considered

- **Chosen: accept when the sync's filters exclude the destination, and refuse changes that would undo it.** The user's existing filter is the proof. Nothing about the sync changes.
- syncfast adds the exclusion itself to every sync that covers a new destination. Rejected: a changed filter holds the sync for a new review (ADR 0002) and rewrites a setting the user owns.
- A destination outside every sync only (today). Rejected: an account whose root is synced can hold no backup.

## Constraints

- The check reuses the filters text a round renders and the existing glob conventions of `filters.rs` (anchored `/X`, unanchored `**/X`, `literal_separator`). It does not run rclone and does not invent new filter semantics.
- One validation path: the check lives with the overlap checks in `backup/config.rs`, and every write that can change a sync or a backup (CLI, MCP, GUI) already goes through config validation. Writes are refused before anything is saved.
- ADR 0009, Destination: amend the rule to say "outside every sync, unless that sync's filters exclude it (see this spec)". Update `docs/guide/backup.md` and `docs/agents.md` where they state the destination rule.

## Risk for the builder to check

The Drive changes API (ADR 0011) watches the account. Daily restic writes under `/Backups` must not start rounds of the `Drive` sync. If changes under an excluded path do start a round today, filter them out before the round is scheduled. A round there would be harmless, but it would show activity on every backup.

## Left to the builder

- How exclusion is computed, within the rule above (a method on `DirMatcher`, or a small function next to it).
- Message wording beyond the examples, error plumbing, and where the sandbox check sits relative to today's missing-folder check.
- Tests: core validation cases (dir rule, `/**` rule, unanchored rule, `+` rule refusal, D equal to root, local destination, filter change refused, mode change refused), CLI exit codes, and the owner's case above as a named test.
