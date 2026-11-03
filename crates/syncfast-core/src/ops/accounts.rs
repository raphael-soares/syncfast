//! The rclone accounts. Names and types only: no secret ever leaves
//! `rclone config dump`'s in-memory parse.

use serde::Serialize;

use super::{OpError, Ops};

const SHARED_CLIENT_ID: &str = "Uses rclone's shared client ID, which stops working in 2026.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountView {
    pub name: String,
    /// The rclone backend: `drive`, `local`, `alias`...
    pub kind: String,
    /// A `drive` account that uses rclone's shared client ID.
    pub shared_client_id: bool,
    pub warning: Option<&'static str>,
}

impl Ops {
    /// Every account rclone knows, with its type and whether a Drive one
    /// uses the shared client ID. Creating an account or changing its
    /// credentials is for the user.
    ///
    /// # Errors
    /// rclone cannot list the accounts or read its config.
    pub async fn accounts(&self) -> Result<Vec<AccountView>, OpError> {
        let kinds = self.rclone.remote_types().await?;
        let shared = self.rclone.drive_remotes_without_own_client_id().await?;
        Ok(kinds
            .into_iter()
            .map(|(name, kind)| {
                let shared_client_id = shared.contains(&name);
                AccountView {
                    name,
                    kind,
                    shared_client_id,
                    warning: shared_client_id.then_some(SHARED_CLIENT_ID),
                }
            })
            .collect())
    }
}
