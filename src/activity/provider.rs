use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use anyhow::{Result, bail};

use super::{config::CalendarSourceConfig, ics::IcsProvider, model::ActivityEvent};

pub type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<ActivityEvent>>> + Send + 'a>>;

/// Provider boundary for calendar source adapters.
///
/// Implementations normalize provider-specific data before returning it. The
/// Activity service owns retries, last-known-good state, and merged queries.
pub trait CalendarProvider: Send + Sync {
    fn kinds(&self) -> &'static [&'static str];
    fn load<'a>(&'a self, source: &'a CalendarSourceConfig) -> ProviderFuture<'a>;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<&'static str, Arc<dyn CalendarProvider>>,
}

impl ProviderRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(IcsProvider));
        registry
    }

    pub fn register(&mut self, provider: Arc<dyn CalendarProvider>) {
        for kind in provider.kinds() {
            self.providers.insert(kind, Arc::clone(&provider));
        }
    }

    pub async fn load(&self, source: &CalendarSourceConfig) -> Result<Vec<ActivityEvent>> {
        let Some(provider) = self.providers.get(source.kind.as_str()) else {
            bail!("unsupported calendar source kind {}", source.kind);
        };
        provider.load(source).await
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderRegistry;
    use crate::activity::config::CalendarSourceConfig;

    #[tokio::test]
    async fn rejects_unregistered_provider_kinds() {
        let source = CalendarSourceConfig {
            id: "remote".into(),
            kind: "unknown".into(),
            path: "/tmp/unknown".into(),
            ..CalendarSourceConfig::default()
        };
        assert!(ProviderRegistry::builtins().load(&source).await.is_err());
    }
}
