//! Notification integration owned by the Activity domain.
//!
//! The native server owns expiry, persistence, and signal delivery. SwayNC is
//! retained as an optional compatibility backend.

pub(crate) mod engine;
pub(crate) mod model;
pub(crate) mod persistence;
pub(crate) mod policy;
pub(crate) mod server;
pub(crate) mod service;
mod swaync;
