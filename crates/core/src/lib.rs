//! CallXL core (CLAUDE.md §4–§7, §10): the state machine behind every UI.
//! Exposed to Swift/Kotlin through UniFFI proc-macros; used directly by `callxl-cli`.

uniffi::setup_scaffolding!();

pub mod api;
mod chat;
pub mod config;
mod diag;
mod engine;
mod files;
mod identity;
mod link;
pub mod logring;
mod relay_client;
mod store;
pub mod types;

pub use api::{CallXl, EventListener};
pub use engine::DevOptions;
pub use types::*;

pub use callxl_media::IcePolicy;
