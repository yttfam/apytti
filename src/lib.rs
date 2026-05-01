pub mod attachments;
pub mod backend;
pub mod config;
pub mod customizations;
mod error;
pub mod handler;
pub mod install;
pub mod models;
pub mod persist;
pub mod registry;
mod schema;
mod server;
pub mod sessions;
pub mod setup;
pub mod stream;

#[cfg(target_os = "macos")]
pub mod macos_menu;

pub use backend::{dispatch, AskRequest, BackendKind, Response};
pub use config::{Cli, Config};
pub use persist::{BackendConfig, HermyttConfig, PersistedConfig};
pub use server::build_router;
