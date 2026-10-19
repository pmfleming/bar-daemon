pub(crate) mod astronomy;
pub(crate) mod config;
pub(crate) mod catalog;
pub(crate) mod place_search;
pub(crate) mod contract_examples;
mod day;
mod ics;
pub(crate) mod locations;
pub(crate) mod model;
pub(crate) mod notifications;
pub(crate) mod provider;
mod service;
mod weather;

pub(crate) use service::ActivityService;
