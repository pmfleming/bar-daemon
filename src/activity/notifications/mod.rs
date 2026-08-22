//! Notification integration owned by the Activity domain.
//!
//! SwayNC remains the active adapter during the staged migration to the native
//! `org.freedesktop.Notifications` server described in the Activity plan.

pub(crate) mod engine;
pub(crate) mod model;
pub(crate) mod persistence;
pub(crate) mod policy;
pub(crate) mod server;
pub(crate) mod service;
mod swaync;

pub(crate) use swaync::monitor;
