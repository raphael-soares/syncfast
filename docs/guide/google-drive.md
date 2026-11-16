# Google Drive

syncfast reaches Drive through rclone, and Google requires each app to identify itself with an OAuth client. rclone's shared client ID is overloaded and is going away, so syncfast asks for your own. It is free and takes a few minutes. The client ID and secret are stored only in your rclone config.

## Create your own client ID

Open **Settings…**, **Accounts**, **Add Google Drive Account…**, or use **Get Started** on the welcome screen. The **Connect Google Drive** sheet walks through the steps, each with an **Open** link to the right Google Cloud page:

1. Create a Google Cloud project (any name).
2. Turn on the **Google Drive API**.
3. Set up the sign-in screen: choose **External**, then under **Branding** paste the home page and privacy policy addresses the sheet shows (each has a **Copy** button).
4. Under **Audience**, press **Publish App**. While the app stays in **Testing**, Google expires the access every 7 days and syncs stop asking you to reconnect. You do not need Google's verification for personal use; the sign-in page shows an "unverified app" warning that you can continue past.
5. Create a client of type **Desktop app** and download its JSON file.
6. Press **Choose Client File…** and pick the downloaded file, or drop it on the window. The sheet reads the client ID and secret from it.

If you would rather type them, open **Enter Manually** and paste the **Client ID** (it ends in `.apps.googleusercontent.com`) and the **Client secret**. rclone's [own guide](https://rclone.org/drive/#making-your-own-client-id) covers the same steps.

## Connect it in syncfast

Press **Connect** and approve access in the browser. You do not name the account: the first one is called `drive`, the next `drive-2`, and so on. Once signed in, syncfast shows the account by its Google address.

An account that still uses rclone's shared client ID shows a warning. Press **Use My Client ID…** next to it and enter yours; the sign-in is repeated. An account that already has its own client ID offers **Change Client ID…** instead.

If the sign-in works but Google Drive then refuses the account (for example the Drive API is not enabled in the project), syncfast shows Google's message under the fields and keeps the new client ID, secret and sign-in. Fix the cause in the Google Cloud console and the account works without signing in again; the status line under the account shows when it does. A sign-in that fails or is cancelled while adding an account leaves no half-made account behind.

## Account status

Every time you open **Settings…**, **Accounts**, syncfast asks each account whether it can reach its files. Each account shows its Google address with its name below, and a status line that says:

- **Checking…** while it asks.
- **Signed in** when it works.
- **Signed out. Reconnect to resume syncing.** when Google rejected the saved token.
- **Offline. Not checked.** when the computer has no network.
- **Problem:** followed by Google's message, for a failure that signing in again does not fix, such as **Error 403: Google Drive API has not been used in project …**. Enable the API in the Google Cloud console and open the pane again.

## Reconnect

When Google rejects the saved token (revoked access, expired testing token), the account shows **Signed out. Reconnect to resume syncing.** Press **Reconnect…** and sign in again. Syncs resume by themselves.

## Rename an account

Press **Rename…**, type the new name and the client ID and secret, and sign in again. rclone cannot rename an account, so syncfast creates the new one, moves your syncs to it and removes the old one. Each moved sync asks for a review before its next round, because the saved listings of the old name no longer apply. No file is deleted.

## Remove an account

Press **Remove…** and confirm. syncfast forgets the account; files on Google Drive are not deleted. Syncs that used it stay in the list and stop with the message that the account is not set up in rclone, until an account with that name is back.

## Several accounts

Add each account the same way; syncfast names them `drive`, `drive-2` and so on, and **Rename…** changes the name. A sync belongs to one account. You can reuse one client file for all of them.

## Google Docs

Docs, Sheets, Slides, Forms, Drawings, My Maps and Sites have no file to download, so syncfast shows each one as a small link file named after it: `Minutes.gdoc.html`, `Budget.gsheet.html`, `Pitch.gslides.html`. Double-click a link to open the file in the browser.

In a two-way sync, what you do with a link happens to the Google file itself:

- **Delete** the link, or the folder it is in, and the file goes to the Drive trash. You can bring it back from **Recently Deleted**.
- **Rename** or **move** the link and the same file is renamed or moved on Drive. Sharing and comments stay.
- **Copy** the link and the file is copied on Drive; the copied link then opens the new copy.

Changes made on Drive come back the same way: a new, renamed, moved or deleted file gets its link added, renamed, moved or removed. If you renamed a link while the file was also renamed on Drive, Drive's name wins and your link is kept next to it as a conflict file.

Deleting many Docs at once stops for review, like deleting many files. In **Download only** and **Mirror to this computer** syncs the links are read-only and nothing you do to them changes Drive. Two files with the same name in one folder get `(2)`, `(3)` and so on.

To stop seeing links, turn off **Show Google Docs as links** in the sync's options; syncfast deletes the links it wrote. The links never upload to Drive as files.
