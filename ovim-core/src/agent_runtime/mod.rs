//! Transient orchestration state projected onto the append-only run log.

#[cfg(test)]
pub(crate) mod test_support;

mod approval;
mod dispatch;
pub mod fake_provider;
mod handoff;
mod headless;
mod loop_runner;
mod mailbox;
mod model_catalog;
mod policy;
mod profile_provider;
mod projection;
mod service;
mod supervisor;
mod workspace;
mod workspace_layout;

pub use approval::*;
pub use dispatch::*;
pub use handoff::*;
pub use headless::*;
pub use loop_runner::*;
pub use mailbox::*;
pub use model_catalog::*;
pub use policy::*;
pub use profile_provider::*;
pub use projection::*;
pub use service::*;
pub use supervisor::*;
pub use workspace::*;
pub use workspace_layout::*;
