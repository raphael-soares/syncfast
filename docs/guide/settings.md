# Settings

Open Settings with `Ctrl+,` or from the tray menu. It is a dialog over the window, with panes grouped under **Syncs** and **App**.

## Syncs

- **Accounts** lists the cloud accounts and signs them in or out.
- **Defaults** holds the options every sync inherits. A sync can override any of them in the **Options** tab of its detail view. Under each row, the caption says whether the value is the built-in one, your default, or overridden by some syncs. **Reset to Default** appears when a value was changed.

## Sync modes

The **Mode** section of a sync's **Options** tab chooses what the sync does. It has no default: every sync sets its own, and **Add Sync** always starts with **Two-way**.

- **Two-way**: changes on either side are copied to the other, and both versions are kept when both changed.
- **Upload only**: new and changed files go to the cloud. Nothing is deleted there and nothing comes back.
- **Download only**: new and changed files come to this computer. Nothing is deleted here.
- **Mirror to Drive**: the cloud folder becomes a copy of this one. Files that exist only in the cloud are deleted. Replaced and deleted files are kept in `.syncfast-versions` on the account.
- **Mirror to this computer**: this folder becomes a copy of the cloud one. Files that exist only here are deleted. They are kept in the versions folder.

Changing the mode holds the sync for a review of its next round. A mirror never runs from an empty source folder: the sync needs attention and the destination keeps every file. Watching for changes only exists in modes that read this computer, conflicts only in **Two-way**, and the rows that do nothing in a mode are hidden. Going back to **Two-way** asks for a review that rebuilds the comparison between both sides.

A mirror that stops at the deletion cap has already deleted up to that many files before the stop. The sync says how many, and the review shows what is left. Those files are kept as versions when versions are on.

## App

- **General**: **Start syncfast when you log in** starts syncfast hidden in the tray when you log in. It is available in the Flatpak install, where the desktop is asked through the Background portal. If the desktop refuses, the row says so and the setting stays as you chose it. On Hyprland the desktop does not run the entry, so the row tells you to start `syncfast-gui --start-hidden` from your own Hyprland config.
- **Notifications**: choose which notices appear.
  - **Needs action** is on by default: a sync is waiting for a review, a resync or another decision.
  - **Conflicts** is on by default: a round kept two versions of a file.
  - **Failures** is the number of failed rounds in a row before the "Failing" notice appears (3 by default). 0 turns that notice off; the sync still shows as Failing in the list after 3 failed rounds.
  - **Success** is off by default. When on, a round that changed files says how many were uploaded, downloaded and deleted. It also tells when syncfast confirmed a review by itself and the round deleted files.
- **Appearance**: **Theme** is System, Light, Dark or one of your theme files.

The same keys are available from the command line (`syncfast settings`) and to agents; see [agents.md](../agents.md).
