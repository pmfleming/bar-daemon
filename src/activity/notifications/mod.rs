//! Notification integration owned by the Activity domain.
//!
//! SwayNC remains the active adapter during the staged migration to the native
//! `org.freedesktop.Notifications` server described in the Activity plan.

pub mod engine;
pub mod model;
pub mod persistence;
pub mod policy;
pub mod server;
pub mod service;
mod swaync;

pub use swaync::monitor;
