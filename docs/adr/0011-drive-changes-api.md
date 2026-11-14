# 0011: Changes on Drive wake a sync through the Drive changes API

## Context
`syncfast watch` only saw the local folder. A change made on Drive (the web, another computer) waited for the timer. The wish: both computers see each other's work within about a minute.

`rclone test changenotify` does not serve. The Drive `changeNotifyRunner` (`backend/drive/drive.go`, v1.76.0) builds a path only when the parent folder is already in the process `dirCache`, and the command never lists anything, so a change in a subfolder produces no notice.

## Decision
- syncfast calls `drive/v3` itself, from `drive_changes.rs`: `changes/startPageToken` once, then `changes` every `REMOTE_POLL` (30 s) with `fields=nextPageToken,newStartPageToken,changes(fileId,removed,file(mimeType,parents))`. A change whose `fileId` or one of its `parents` is a folder of a sync marks that sync; a folder found that way joins the set. `removed` without a `file` carries no parents, so it marks every sync of the account.
- One poller per Drive account with at least one enabled sync that has `watch` on and reads from Drive (`bisync`, `mirror_down`, `download`). The folder IDs of a sync come from `rclone lsjson -R --dirs-only --fast-list`, and the root of the sync from its parent listing. A shared drive passes `driveId`, `supportsAllDrives` and `includeItemsFromAllDrives`; My Drive passes `restrictToMyDrive=true`.
- The token is the one rclone saved, read from `rclone config dump` (never logged, kept in `Zeroizing`, redacted in `Debug`). When it is expired or Google answers 401, syncfast runs `rclone about <account>:`, which renews the token and writes it to the rclone config, and reads it again. syncfast never writes a token.
- HTTP is `ureq` 3 with `rustls` and the `ring` provider, in `spawn_blocking`. Certificates come from the system through `rustls-platform-verifier`; `webpki-roots` is not used (its CDLA licence is outside `deny.toml`).
- A network or HTTP error keeps the page token and tries again next cycle, logging once per run of failures. HTTP 400, 404 and 410 drop the session, which is rebuilt next cycle.
- Resume from suspend (`PrepareForSleep(false)` from logind) and the network coming back (NetworkManager `StateChanged` crossing 70) make every enabled sync due, watched or not. The timer is a safety net (the monotonic timer pauses in suspend).

## Consequences
- A change made by this computer returns through the feed and wakes one extra round without changes. It is cheap and, with idle logs, does not fill the history.
- A change that touches a file that is not under a known folder, and is not a removal, is missed until the next full check.
- The Flatpak needs `--system-talk-name=org.freedesktop.login1` (ADR 0006).
- The real signal path and the real HTTP calls are not covered by tests (no system bus or Google in CI); the mapping of a response to syncs, the wake targets and the debounce are.
