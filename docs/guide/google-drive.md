# Google Drive

syncfast reaches Drive through rclone, and Google requires each app to identify itself with an OAuth client. rclone's shared client ID is overloaded and is going away, so syncfast asks for your own. It is free and takes a few minutes. The client ID and secret are stored only in your rclone config.

## Create your own client ID

The canonical steps are in [rclone's guide](https://rclone.org/drive/#making-your-own-client-id). In short:

1. Open the [Google Cloud console](https://console.cloud.google.com/) and create a project (any name).
2. Enable the **Google Drive API** for it (APIs and Services, Library).
3. Configure the **OAuth consent screen**. Choose **External**, fill in the app name and your email, and add your own Google account as a test user if asked.
4. Set the publishing status to **In production**. While it stays in **Testing**, Google expires the access every 7 days and syncs stop asking you to reconnect. You do not need Google's verification for personal use; the sign-in page shows an "unverified app" warning that you can continue past.
5. Under **Credentials**, create an **OAuth client ID** of type **Desktop app**.
6. Copy the **Client ID** (it ends in `.apps.googleusercontent.com`) and the **Client secret**.

## Connect it in syncfast

Open **Settings…**, **Accounts**, **Add Google Drive Account…**, or use **Get Started** on the welcome screen. Enter an account name, the client ID and the secret, press **Connect** and approve access in the browser.

An account that still uses rclone's shared client ID shows a warning. Press **Use My Client ID…** next to it and enter yours; the sign-in is repeated.

## Reconnect

When Google rejects the saved token (revoked access, expired testing token), the account shows **Signed out. Reconnect to resume syncing.** Press **Reconnect…** and sign in again. Syncs resume by themselves.

## Several accounts

Add each account with its own name. A sync belongs to one account. You can reuse one client ID for all of them.
