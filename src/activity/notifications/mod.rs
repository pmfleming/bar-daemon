//! Notification integration owned by the Activity domain.
//!
//! SwayNC remains the active adapter during the staged migration to the native
//! `org.freedesktop.Notifications` server described in the Activity plan.

pub mod engine;
pub mod model;
pub mod policy;
pub mod server;
mod swaync;

pub use swaync::{monitor, toggle_dnd, toggle_panel};
