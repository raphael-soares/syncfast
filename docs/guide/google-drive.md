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
