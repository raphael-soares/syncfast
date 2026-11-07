# Settings

Open Settings with `Ctrl+,` or from the tray menu. It is a dialog over the window, with panes grouped under **Syncs** and **App**.

## Syncs

- **Accounts** lists the cloud accounts and signs them in or out.
- **Defaults** holds the options every sync inherits. A sync can override any of them in the **Options** tab of its detail view. Under each row, the caption says whether the value is the built-in one, your default, or overridden by some syncs. **Reset to Default** appears when a value was changed.

## App

- **General**: **Start syncfast when you log in** starts syncfast hidden in the tray when you log in. It is available in the Flatpak install, where the desktop is asked through the Background portal. If the desktop refuses, the row says so and the setting stays as you chose it. On Hyprland the desktop does not run the entry, so the row tells you to start `syncfast-gui --start-hidden` from your own Hyprland config.
- **Notifications**: choose which notices appear.
  - **Needs action** is on by default: a sync is waiting for a review, a resync or another decision.
  - **Conflicts** is on by default: a round kept two versions of a file.
  - **Failures** is the number of failed rounds in a row before the "Failing" notice appears (3 by default). 0 turns that notice off; the sync still shows as Failing in the list after 3 failed rounds.
  - **Success** is off by default. When on, a round that changed files says how many were uploaded, downloaded and deleted.
- **Appearance**: **Theme** is System, Light, Dark or one of your theme files.

The same keys are available from the command line (`syncfast settings`) and to agents; see [agents.md](../agents.md).
