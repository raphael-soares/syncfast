//! Native Google files as link files on disk, kept by Drive file id
//! (ADR 0010). rclone never sees them; this module plans and applies them.

mod apply;
pub mod link;
pub mod plan;
pub mod remote;
pub mod state;

pub(crate) use apply::remote_path;
pub use apply::{Context, DocsError, DocsOutcome, NOT_LINKED, Phase, reconcile};

/// Keeps link files, and rclone's conflict names for them, out of rclone.
pub const LINK_FILTER: &str = "- *.{gdoc,gsheet,gslides,gform,gdraw,gmap,gsite}.html*";
