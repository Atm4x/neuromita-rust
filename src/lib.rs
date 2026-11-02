pub mod adapters;
pub mod api;
pub mod app;
pub mod character;
pub mod generation;
mod identity;
mod interaction;
pub mod knowledge;
pub mod launcher;
pub mod lifecycle;
// Administrative workflows are contract-tested; an authenticated control route is not yet wired.
#[cfg_attr(not(test), allow(dead_code))]
mod maintenance;
pub mod migration;
pub mod presentation;
pub mod request;
mod runtime;
pub mod server;
pub mod settings;
pub mod timeline;
pub mod voice;

pub mod memory;
pub mod turn;
