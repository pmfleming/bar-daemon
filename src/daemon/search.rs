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

#[derive(Clone)]
pub(super) struct Search {
    catalogs: Arc<Mutex<Catalogs>>,
    workers: Arc<Semaphore>,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            catalogs: Arc::default(),
            workers: Arc::new(Semaphore::new(2)),
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
                    return error("validation-error", "search catalog exceeds its bounds");
                }
                let fingerprint: [u8; 32] =
                    Sha256::digest(serde_json::to_vec(&request.items).unwrap()).into();
                let mut catalogs = self.catalogs.lock().unwrap();
                let fresh = !catalogs.contains_key(owner.name());
                if fresh && catalogs.len() >= MAX_CLIENTS {
                    return error("busy", "search client capacity reached");
                }
                let client = catalogs.entry(owner.name().into()).or_default();
                if let Some(previous) = client.get(&request.owner)
                    && previous.revision >= request.revision
                {
                    if previous.revision == request.revision && previous.fingerprint == fingerprint
                    {
                        return success(
                            json!({"owner":request.owner, "revision":request.revision}),
                        );
                    }
                    return error("stale-catalog", "search catalog revision must increase");
                }
                if (!client.contains_key(&request.owner) && client.len() >= MAX_CATALOGS)
                    || client
                        .iter()
                        .filter(|(key, _)| *key != &request.owner)
                        .map(|(_, catalog)| catalog.bytes)
                        .sum::<usize>()
                        + bytes
                        > MAX_BYTES
                {
                    return error("busy", "search catalog capacity reached");
                }
                client.insert(
                    request.owner.clone(),
                    Catalog {
                        revision: request.revision,
                        bytes,
                        fingerprint,
                        prepared: Arc::new(PreparedCatalog::new(&request.items)),
                    },
                );
                if fresh {
                    let owner = owner.clone();
                    let catalogs = self.catalogs.clone();
                    tokio::spawn(async move {
                        owner.closed().await;
                        catalogs.lock().unwrap().remove(owner.name());
                    });
                }
                success(json!({"owner":request.owner, "revision":request.revision}))
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
                    let catalogs = self.catalogs.lock().unwrap();
                    let Some(catalog) = catalogs
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
                if let Some(client) = self.catalogs.lock().unwrap().get_mut(owner.name()) {
                    client.remove(&request.owner);
                }
                success(json!({"owner":request.owner}))
            }
            _ => crate::api::unsupported_method(method),
        }
    }
}
fn valid_owner(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 128
}

#[cfg(test)]
mod tests;
