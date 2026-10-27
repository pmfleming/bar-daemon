pub(crate) mod agenda;
mod agenda_adapter;
mod agenda_projection;
pub(crate) mod agenda_schedule;
pub(crate) mod agenda_store;
#[cfg(test)]
mod agenda_tests;
pub(crate) mod astronomy;
pub(crate) mod catalog;
pub(crate) mod config;
pub(crate) mod contract_examples;
mod day;
mod ics;
pub(crate) mod locations;
pub(crate) mod model;
pub(crate) mod notifications;
pub(crate) mod place_search;
pub(crate) mod presentation;
pub(crate) mod provider;
mod service;
mod weather;

pub(crate) use service::ActivityService;
