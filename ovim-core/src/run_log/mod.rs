//! Provider-independent, append-only history for agent runs.
//!
//! This module intentionally starts with an in-memory implementation. Durable
//! stores can implement [`RunEventSink`] without changing the event vocabulary
//! consumed by editor projections.

mod artifact;
mod catalog;
mod event;
mod identity;
mod layout;
mod liveness;
mod local_store;
mod manifest;
mod manifest_git;
mod query;
mod recovery;
mod replay;
mod repository;
mod retention;
mod sqlite;
mod store;
mod workspace_capture;

pub use artifact::*;
pub use catalog::*;
pub use event::*;
pub use identity::*;
pub use layout::*;
pub(crate) use liveness::*;
pub use local_store::*;
pub use manifest::*;
pub use manifest_git::*;
pub use query::*;
pub use recovery::*;
pub use replay::*;
pub use repository::*;
pub use retention::*;
pub use sqlite::*;
pub use store::*;
pub use workspace_capture::*;
