// Pre-existing clippy lints under rust 1.95 — file under cleanup, not blocking shipment.
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::needless_return)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::type_complexity)]
#![allow(clippy::cloned_ref_to_slice_refs)]

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
pub mod update;

/// Port the HTTP server bound to, recorded at startup.
///
/// The self-update relaunch helper needs it to health-probe the new build, and
/// that helper is generated after the port is long out of scope.
static SERVER_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(7781);

pub fn set_update_port(port: u16) {
    SERVER_PORT.store(port, std::sync::atomic::Ordering::Relaxed);
}

pub fn update_port() -> u16 {
    SERVER_PORT.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(target_os = "macos")]
pub mod macos_menu;

pub use backend::{dispatch, AskRequest, BackendKind, Response};
pub use config::{Cli, Config};
pub use persist::{BackendConfig, HermyttConfig, PersistedConfig};
pub use server::build_router;
