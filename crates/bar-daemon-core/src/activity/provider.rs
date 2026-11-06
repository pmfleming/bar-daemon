use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use anyhow::{Result, bail};

use super::{config::CalendarSourceConfig, ics::IcsProvider, model::ActivityEvent};

pub(crate) type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ActivityEvent>>> + Send + 'a>>;

/// Provider boundary for calendar source adapters.
///
/// Implementations normalize provider-specific data before returning it. The
/// Activity service owns retries, last-known-good state, and merged queries.
pub trait CalendarProvider: Send + Sync {
    fn kinds(&self) -> &'static [&'static str];
    fn load<'a>(&'a self, source: &'a CalendarSourceConfig) -> ProviderFuture<'a>;
}

#[derive(Clone, Default)]
pub(crate) struct ProviderRegistry {
    providers: HashMap<&'static str, Arc<dyn CalendarProvider>>,
}

impl ProviderRegistry {
    pub(crate) fn builtins() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(IcsProvider));
        registry
    }

    pub(crate) fn register(&mut self, provider: Arc<dyn CalendarProvider>) {
        for kind in provider.kinds() {
            self.providers.insert(kind, Arc::clone(&provider));
        }
    }

    pub(crate) async fn load(&self, source: &CalendarSourceConfig) -> Result<Vec<ActivityEvent>> {
        let Some(provider) = self.providers.get(source.kind.as_str()) else {
            bail!("unsupported calendar source kind {}", source.kind);
        };
        let events = provider.load(source).await?;
        anyhow::ensure!(
            events.len() <= 10000,
            "calendar source exceeds 10000 events"
        );
        let mut ids = std::collections::HashSet::new();
        for event in &events {
            super::agenda_adapter::imported(event)?;
            anyhow::ensure!(ids.insert(&event.id), "duplicate calendar event identity");
        }
        Ok(events)
    }
}
