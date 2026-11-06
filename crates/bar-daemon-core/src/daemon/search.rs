//! Ephemeral, transport-owned result catalogs. No persistence or domain effects.
use super::owner::Owner;
use crate::api::{decode_request, error, success};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shelllist_search::{PreparedCatalog, SearchItem};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

const MAX_CLIENTS: usize = 64;
const MAX_CATALOGS: usize = 128;
const MAX_ITEMS: usize = 20_000;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRequest {
    owner: String,
    revision: u64,
    items: Vec<SearchItem>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryRequest {
    owner: String,
    revision: u64,
    generation: u64,
    query: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    owner: String,
}

struct Catalog {
    revision: u64,
    bytes: usize,
    fingerprint: [u8; 32],
    prepared: Arc<PreparedCatalog>,
}
type Catalogs = HashMap<String, HashMap<String, Catalog>>;
#[derive(Default)]
struct Store {
    clients: Catalogs,
    pending: HashMap<(String, String), (u64, u64)>,
    next: u64,
}
struct Preparation {
    store: Arc<Mutex<Store>>,
    key: (String, String),
    ticket: u64,
}
impl Drop for Preparation {
    fn drop(&mut self) {
        let mut store = self.store.lock().unwrap();
        if store
            .pending
            .get(&self.key)
            .is_some_and(|(ticket, _)| *ticket == self.ticket)
        {
            store.pending.remove(&self.key);
        }
    }
}

#[derive(Clone)]
pub(super) struct Search {
    catalogs: Arc<Mutex<Store>>,
    workers: Arc<Semaphore>,
    #[cfg(test)]
    preparation_barrier: Arc<Mutex<Option<Arc<std::sync::Barrier>>>>,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            catalogs: Arc::default(),
            workers: Arc::new(Semaphore::new(2)),
            #[cfg(test)]
            preparation_barrier: Arc::default(),
        }
    }
}
impl Search {
    pub(super) async fn call(&self, method: &str, params: Value, owner: &Owner) -> Value {
        if !owner.exists().await {
            return error("owner-lost", "search caller disconnected");
        }
        match method {
            "search.catalog" => {
                let request = match decode_request::<CatalogRequest>(params, method) {
                    Ok(request) => request,
                    Err(response) => return response,
                };
                if !valid_owner(&request.owner)
                    || request.revision == 0
                    || request.items.len() > MAX_ITEMS
                {
                    return error(
                        "validation-error",
                        "invalid search owner, revision or catalog size",
                    );
                }
                let Ok(permit) = self.workers.clone().try_acquire_owned() else {
                    return error("busy", "search workers are busy");
                };
                let key = (owner.name().to_owned(), request.owner.clone());
                let ticket = {
                    let mut store = self.catalogs.lock().unwrap();
                    if store
                        .pending
                        .get(&key)
                        .is_some_and(|(_, revision)| *revision >= request.revision)
                    {
                        return error("busy", "catalog revision is already preparing");
                    }
                    let fresh = !store.clients.contains_key(owner.name());
                    if fresh && store.clients.len() >= MAX_CLIENTS {
                        return error("busy", "search client capacity reached");
                    }
                    store.clients.entry(owner.name().into()).or_default();
                    store.next = store.next.wrapping_add(1);
                    let ticket = store.next;
                    store
                        .pending
                        .insert(key.clone(), (ticket, request.revision));
                    if fresh {
                        let owner = owner.clone();
                        let catalogs = self.catalogs.clone();
                        tokio::spawn(async move {
                            owner.closed().await;
                            let mut store = catalogs.lock().unwrap();
                            store.clients.remove(owner.name());
                            store
                                .pending
                                .retain(|(transport, _), _| transport != owner.name());
                        });
                    }
                    ticket
                };
                let preparation = Preparation {
                    store: self.catalogs.clone(),
                    key: key.clone(),
                    ticket,
                };
                #[cfg(test)]
                let barrier = self.preparation_barrier.lock().unwrap().clone();
                let prepared = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    #[cfg(test)]
                    if let Some(barrier) = barrier {
                        barrier.wait();
                    }
                    prepare_catalog(request)
                })
                .await;
                let (logical_owner, catalog) = match prepared {
                    Ok(Ok(result)) => result,
                    Ok(Err(response)) => return response,
                    Err(_) => return error("internal-error", "search preparation worker failed"),
                };
                if !owner.exists().await {
                    return error(
                        "owner-lost",
                        "search caller disconnected during preparation",
                    );
                }
                let mut store = self.catalogs.lock().unwrap();
                if store.pending.get(&key).map(|(ticket, _)| *ticket) != Some(preparation.ticket) {
                    return error(
                        "catalog-released",
                        "catalog was released or superseded during preparation",
                    );
                }
                store.pending.remove(&key);
                let catalogs = &mut store.clients;
                let fresh = !catalogs.contains_key(owner.name());
                if fresh && catalogs.len() >= MAX_CLIENTS {
                    return error("busy", "search client capacity reached");
                }
                let client = catalogs.entry(owner.name().into()).or_default();
                if let Some(previous) = client.get(&logical_owner)
                    && previous.revision >= catalog.revision
                {
                    if previous.revision == catalog.revision
                        && previous.fingerprint == catalog.fingerprint
                    {
                        return success(
                            json!({"owner":logical_owner, "revision":catalog.revision}),
                        );
                    }
                    return error("stale-catalog", "search catalog revision must increase");
                }
                if (!client.contains_key(&logical_owner) && client.len() >= MAX_CATALOGS)
                    || client
                        .iter()
                        .filter(|(key, _)| *key != &logical_owner)
                        .map(|(_, catalog)| catalog.bytes)
                        .sum::<usize>()
                        + catalog.bytes
                        > MAX_BYTES
                {
                    return error("busy", "search catalog capacity reached");
                }
                let revision = catalog.revision;
                client.insert(logical_owner.clone(), catalog);
                drop(store);
                drop(preparation);
                success(json!({"owner":logical_owner, "revision":revision}))
            }

            "search.query" => {
                let request = match decode_request::<QueryRequest>(params, method) {
                    Ok(request) => request,
                    Err(response) => return response,
                };
                if !valid_owner(&request.owner) || request.query.len() > 4096 {
                    return error("validation-error", "invalid search owner or query size");
                }
                let prepared = {
                    let store = self.catalogs.lock().unwrap();
                    let Some(catalog) = store
                        .clients
                        .get(owner.name())
                        .and_then(|client| client.get(&request.owner))
                    else {
                        return error(
                            "catalog-not-found",
                            "search catalog was released or caller changed",
                        );
                    };
                    if catalog.revision != request.revision {
                        return error("stale-catalog", "query does not target the current catalog");
                    }
                    catalog.prepared.clone()
                };
                let Ok(permit) = self.workers.clone().try_acquire_owned() else {
                    return error("busy", "search workers are busy");
                };
                let result = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let response = prepared.rank(request.owner, request.generation, &request.query);
                    success(
                        json!({"owner":response.owner, "generation":response.generation,
                        "revision":request.revision, "keys":response.keys}),
                    )
                })
                .await;
                result.unwrap_or_else(|_| error("internal-error", "search worker failed"))
            }
            "search.release" => {
                let request = match decode_request::<ReleaseRequest>(params, method) {
                    Ok(request) => request,
                    Err(response) => return response,
                };
                let mut store = self.catalogs.lock().unwrap();
                store
                    .pending
                    .remove(&(owner.name().into(), request.owner.clone()));
                if let Some(client) = store.clients.get_mut(owner.name()) {
                    client.remove(&request.owner);
                }
                success(json!({"owner":request.owner}))
            }
            _ => crate::api::unsupported_method(method),
        }
    }
}
fn prepare_catalog(request: CatalogRequest) -> Result<(String, Catalog), Value> {
    let bytes = request
        .items
        .iter()
        .map(|item| {
            std::mem::size_of::<SearchItem>()
                + item.key.len()
                + item.title.len()
                + item.subtitle.len()
                + item
                    .keywords
                    .iter()
                    .map(|keyword| keyword.len() + std::mem::size_of::<String>())
                    .sum::<usize>()
        })
        .sum::<usize>();
    if bytes > MAX_BYTES
        || request
            .items
            .iter()
            .any(|item| !item.score.is_finite() || !item.provider_priority.is_finite())
    {
        return Err(error(
            "validation-error",
            "search catalog exceeds its bounds",
        ));
    }
    let fingerprint = Sha256::digest(serde_json::to_vec(&request.items).unwrap()).into();
    let prepared = Arc::new(PreparedCatalog::new(&request.items));
    Ok((
        request.owner,
        Catalog {
            revision: request.revision,
            bytes,
            fingerprint,
            prepared,
        },
    ))
}

fn valid_owner(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 128
}

#[cfg(test)]
mod tests;
