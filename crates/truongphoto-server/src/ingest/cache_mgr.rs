use chrono::Utc;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Weak};
use truongphoto_core::LruCacheEntry;
use truongphoto_db::{repo, DbHandle};

const TOUCH_THROTTLE_MS: i64 = 60_000;

/// Manages singleflight generation locks and bounded LRU disk cache eviction for `cache_dir`.
pub struct CacheManager {
    db: DbHandle,
    locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    last_touched: Mutex<HashMap<String, i64>>,
}

impl CacheManager {
    pub fn new(db: DbHandle) -> Self {
        Self {
            db,
            locks: Mutex::new(HashMap::new()),
            last_touched: Mutex::new(HashMap::new()),
        }
    }

    /// Acquires an async singleflight mutex guard for `key` to prevent duplicate concurrent work.
    pub async fn singleflight_lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = {
            let mut map = self.locks.lock();
            if map.len() > 512 {
                map.retain(|_, weak| weak.strong_count() > 0);
            }
            if let Some(existing) = map.get(key).and_then(Weak::upgrade) {
                existing
            } else {
                let created = Arc::new(tokio::sync::Mutex::new(()));
                map.insert(key.to_string(), Arc::downgrade(&created));
                created
            }
        };
        mutex.lock_owned().await
    }

    /// Upserts an `lru_cache_entries` row and evicts oldest entries down to 85% of `cache_limit_gb` if exceeded.
    pub async fn record_and_evict(
        &self,
        entry: LruCacheEntry,
        cache_dir: &Path,
        cache_limit_gb: u32,
    ) -> anyhow::Result<()> {
        let key = entry.cache_key.clone();
        let now_ms = entry.last_accessed_ms;
        self.db
            .write(move |conn| repo::cache::upsert_cache_entry(conn, &entry))
            .await?;

        self.last_touched.lock().insert(key, now_ms);

        let limit_bytes = (u64::from(cache_limit_gb.max(1))).saturating_mul(1024 * 1024 * 1024);
        let total = self.db.read(repo::cache::total_cache_bytes).await?;

        if total > limit_bytes {
            let target_bytes = (limit_bytes * 85) / 100;
            let bytes_to_free = total.saturating_sub(target_bytes);
            if bytes_to_free > 0 {
                let victims = self
                    .db
                    .write(move |conn| repo::cache::pop_oldest_entries_to_free(conn, bytes_to_free))
                    .await?;

                let cache_root = cache_dir.to_path_buf();
                let victim_paths: Vec<String> =
                    victims.iter().map(|v| v.rel_path.clone()).collect();

                {
                    let mut touched = self.last_touched.lock();
                    for v in &victims {
                        touched.remove(&v.cache_key);
                    }
                }

                tokio::task::spawn_blocking(move || {
                    for rel in victim_paths {
                        if let Some(abs) = safe_join_relative(&cache_root, &rel) {
                            remove_cache_target(&abs);
                        }
                    }
                })
                .await?;
            }
        }

        Ok(())
    }

    /// Updates `last_accessed_ms` in SQLite at most once every 60 seconds per `cache_key`.
    pub async fn touch_throttled(&self, cache_key: &str) {
        let now_ms = Utc::now().timestamp_millis();
        let should_update = {
            let mut map = self.last_touched.lock();
            match map.get(cache_key) {
                Some(&prev) if now_ms.saturating_sub(prev) < TOUCH_THROTTLE_MS => false,
                _ => {
                    map.insert(cache_key.to_string(), now_ms);
                    true
                }
            }
        };

        if should_update {
            let key = cache_key.to_string();
            let _ = self
                .db
                .write(move |conn| repo::cache::touch_cache_entry(conn, &key, now_ms))
                .await;
        }
    }

    /// Deletes all LRU cache entries and on-disk files/directories associated with `asset_id`.
    pub async fn purge_asset_cache(&self, asset_id: &str, cache_dir: &Path) -> anyhow::Result<u64> {
        let aid = asset_id.to_string();
        let entries = self
            .db
            .write(move |conn| repo::cache::remove_cache_entries_for_asset(conn, &aid))
            .await?;

        if entries.is_empty() {
            return Ok(0);
        }

        {
            let mut touched = self.last_touched.lock();
            for e in &entries {
                touched.remove(&e.cache_key);
            }
        }

        let cache_root = cache_dir.to_path_buf();
        let rels: Vec<String> = entries.into_iter().map(|e| e.rel_path).collect();
        let removed = tokio::task::spawn_blocking(move || {
            let mut count = 0u64;
            for rel in rels {
                if let Some(abs) = safe_join_relative(&cache_root, &rel) {
                    if remove_cache_target(&abs) {
                        count += 1;
                    }
                }
            }
            count
        })
        .await?;

        Ok(removed)
    }
}

pub(crate) fn safe_join_relative(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }
    let mut out = root.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Joins `rel` onto `root` while rejecting any symlink traversal or escape outside `root`.
pub(crate) fn safe_join_no_symlinks(root: &Path, rel: &str) -> Option<PathBuf> {
    let joined = safe_join_relative(root, rel)?;
    let canon_root = std::fs::canonicalize(root).ok()?;
    let mut probe = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(seg) => {
                probe.push(seg);
                match std::fs::symlink_metadata(&probe) {
                    Ok(meta) if meta.file_type().is_symlink() => return None,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(joined),
                    Err(_) => return None,
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if let Ok(canon_target) = std::fs::canonicalize(&joined) {
        if !canon_target.starts_with(&canon_root) {
            return None;
        }
    }
    Some(joined)
}

fn remove_cache_target(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                std::fs::remove_dir_all(path).is_ok()
            } else {
                std::fs::remove_file(path).is_ok()
            }
        }
        Err(_) => false,
    }
}
