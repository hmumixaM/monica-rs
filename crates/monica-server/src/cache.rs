//! Bounded per-resource caching. Callers supply versions read from committed
//! entity rows, never a sequence allocation or a whole-site invalidation number.
use crate::auth::ApiError;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, OnceCell, Semaphore};

#[derive(Clone)]
pub struct ReadCache(Arc<Cache>);
struct Cache {
    entries: Mutex<Entries>,
    max_entries: usize,
    max_bytes: usize,
    computations: Semaphore,
}
#[derive(Default)]
struct Entries {
    clock: u64,
    entries: HashMap<(String, String), Entry>,
}
struct Entry {
    slot: Arc<Slot>,
    used: u64,
}
#[derive(Default)]
struct Slot {
    value: OnceCell<Arc<CachedJson>>,
    readers: AtomicUsize,
}
struct Reader(Arc<Slot>);
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct CachedJson {
    body: Bytes,
    etag: HeaderValue,
}
impl CachedJson {
    pub fn response(&self, headers: &HeaderMap) -> Response {
        let etag = self.etag.to_str().expect("ASCII digest");
        let matched = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(',').any(|tag| {
                    let tag = tag.trim();
                    tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag
                })
            });
        let mut response = if matched {
            Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .body(Body::empty())
                .unwrap()
        } else {
            Response::new(Body::from(self.body.clone()))
        };
        response
            .headers_mut()
            .insert(header::ETAG, self.etag.clone());
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Cookie"));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        response
    }
}

impl Default for ReadCache {
    fn default() -> Self {
        Self::new(128, 16 * 1024 * 1024)
    }
}
impl ReadCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self(Arc::new(Cache {
            entries: Mutex::new(Entries::default()),
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
            computations: Semaphore::new(4),
        }))
    }

    /// Authenticate before calling. A scope includes resource, query and any
    /// user-specific policy; version includes every committed dependency of it.
    /// Only one factory runs for a (scope,version), including concurrent misses.
    pub async fn get_or_try_init<F, Fut>(
        &self,
        scope: String,
        version: String,
        build: F,
    ) -> Result<Arc<CachedJson>, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, ApiError>>,
    {
        let key = (scope, version);
        let slot = {
            let mut state = self.0.entries.lock().await;
            state.clock = state.clock.wrapping_add(1);
            let clock = state.clock;
            if !state.entries.contains_key(&key) {
                while state.entries.len() >= self.0.max_entries {
                    if !evict(&mut state) {
                        return Err(ApiError(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "read cache capacity busy",
                        ));
                    }
                }
                state.entries.insert(
                    key.clone(),
                    Entry {
                        slot: Arc::new(Slot::default()),
                        used: clock,
                    },
                );
            }
            let entry = state.entries.get_mut(&key).unwrap();
            entry.used = clock;
            entry.slot.readers.fetch_add(1, Ordering::AcqRel);
            entry.slot.clone()
        };
        let reader = Reader(slot.clone());
        let value =
            slot.value
                .get_or_try_init(|| async {
                    let _permit = self.0.computations.acquire().await.map_err(|_| {
                        ApiError(StatusCode::SERVICE_UNAVAILABLE, "read cache stopped")
                    })?;
                    let value = build().await?;
                    let bytes = serde_json::to_vec(&value).map_err(|_| {
                        ApiError(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "response encoding failed",
                        )
                    })?;
                    if bytes.len() > 8 * 1024 * 1024 {
                        return Err(ApiError(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "response exceeds bounded page size",
                        ));
                    }
                    let etag = HeaderValue::from_str(&format!("\"{:x}\"", Sha256::digest(&bytes)))
                        .unwrap();
                    Ok(Arc::new(CachedJson {
                        body: Bytes::from(bytes),
                        etag,
                    }))
                })
                .await
                .cloned();
        drop(reader);
        let mut state = self.0.entries.lock().await;
        let bytes = || {
            state
                .entries
                .values()
                .filter_map(|e| e.slot.value.get())
                .map(|v| v.body.len())
                .sum::<usize>()
        };
        let mut total = bytes();
        while total > self.0.max_bytes && evict(&mut state) {
            total = state
                .entries
                .values()
                .filter_map(|e| e.slot.value.get())
                .map(|v| v.body.len())
                .sum();
        }
        value
    }
}

fn evict(state: &mut Entries) -> bool {
    let key = state
        .entries
        .iter()
        .filter(|(_, e)| e.slot.readers.load(Ordering::Acquire) == 0)
        .min_by_key(|(_, e)| e.used)
        .map(|(key, _)| key.clone());
    key.is_some_and(|key| state.entries.remove(&key).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn misses_coalesce_versions_are_local_and_etags_are_conditional() {
        let cache = ReadCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..24 {
            let cache = cache.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                cache
                    .get_or_try_init("profile:a".into(), "committed:1".into(), || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        Ok(json!({"name":"a"}))
                    })
                    .await
                    .unwrap()
            }));
        }
        let first = tasks.remove(0).await.unwrap();
        for task in tasks {
            assert!(Arc::ptr_eq(&first, &task.await.unwrap()));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let changed = cache
            .get_or_try_init("profile:a".into(), "committed:2".into(), || async {
                Ok(json!({"name":"b"}))
            })
            .await
            .unwrap();
        assert_ne!(first.etag, changed.etag);
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, first.etag.clone());
        assert_eq!(first.response(&headers).status(), StatusCode::NOT_MODIFIED);
        assert_eq!(changed.response(&headers).status(), StatusCode::OK);
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(&format!("W/{}", changed.etag.to_str().unwrap())).unwrap(),
        );
        assert_eq!(
            changed.response(&headers).status(),
            StatusCode::NOT_MODIFIED
        );
    }
    #[tokio::test]
    async fn cancellation_releases_singleflight_and_storage_is_bounded() {
        let cache = ReadCache::new(2, 128);
        let started = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn({
            let cache = cache.clone();
            let started = started.clone();
            async move {
                cache
                    .get_or_try_init("a".into(), "1".into(), || async {
                        started.notify_one();
                        std::future::pending::<Result<Value, ApiError>>().await
                    })
                    .await
            }
        });
        started.notified().await;
        task.abort();
        let _ = task.await;
        cache
            .get_or_try_init("a".into(), "1".into(), || async { Ok(json!(1)) })
            .await
            .unwrap();
        for i in 0..20 {
            cache
                .get_or_try_init(i.to_string(), "1".into(), || async {
                    Ok(json!("x".repeat(64)))
                })
                .await
                .unwrap();
        }
        let state = cache.0.entries.lock().await;
        assert!(state.entries.len() <= 2);
        assert!(
            state
                .entries
                .values()
                .filter_map(|e| e.slot.value.get())
                .map(|v| v.body.len())
                .sum::<usize>()
                <= 128
        );
    }
}
