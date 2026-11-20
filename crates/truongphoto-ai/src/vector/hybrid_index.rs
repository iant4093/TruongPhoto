use crate::vector::flat_store::{l2_normalize, top_k_sort, FlatVectorStore};
use crate::AiError;
use parking_lot::RwLock;
use rayon::prelude::*;
use roaring::RoaringBitmap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use truongphoto_core::PlannerMode;
use usearch::ffi::{IndexOptions, MetricKind, ScalarKind};
use usearch::Index;

/// Cardinality threshold below which exact parallel SIMD scan runs instead of HNSW.
pub const EXACT_SCAN_THRESHOLD: u64 = 35_000;

/// Delta index size threshold recommending compaction into the mmap-backed base `.usearch` file.
pub const DELTA_COMPACT_THRESHOLD: usize = 20_000;

/// HNSW `efSearch` parameter for high-recall filtered queries.
pub const HNSW_EF_SEARCH: usize = 256;

/// Hybrid vector index combining:
/// - `FlatVectorStore` (`<name>.f16`): canonical mmap-backed `f16` vectors for exact SIMD scan & full recovery
/// - `base` (`<name>.usearch`): read-only `Index::view` mmap HNSW graph
/// - `delta`: mutable in-RAM USearch HNSW graph for recent inserts/updates
/// - `tombstones` (`<name>.tombstones.roar`): persistent `RoaringBitmap` of deleted vector IDs
pub struct HybridVectorIndex {
    dim: usize,
    base_path: PathBuf,
    tombstone_path: PathBuf,
    flat: Arc<FlatVectorStore>,
    base: RwLock<Option<Index>>,
    delta: RwLock<Index>,
    delta_ids: RwLock<RoaringBitmap>,
    tombstones: RwLock<RoaringBitmap>,
}

impl std::fmt::Debug for HybridVectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HybridVectorIndex")
            .field("dim", &self.dim)
            .field("base_path", &self.base_path)
            .field("flat_count", &self.flat.count())
            .field("has_base", &self.has_base())
            .field("delta_len", &self.delta_len())
            .field("tombstones", &self.tombstone_len())
            .finish()
    }
}

impl HybridVectorIndex {
    /// Opens (or initializes) a `HybridVectorIndex` in `dir` under stem `name`.
    pub fn open(dir: &Path, name: &str, dim: usize) -> Result<Self, AiError> {
        std::fs::create_dir_all(dir)?;
        let flat_path = dir.join(format!("{name}.f16"));
        let base_path = dir.join(format!("{name}.usearch"));
        let tombstone_path = dir.join(format!("{name}.tombstones.roar"));

        let flat = Arc::new(FlatVectorStore::open(&flat_path, dim)?);
        let tombstones = load_tombstones(&tombstone_path)?;
        let base = if base_path.is_file() {
            Some(open_base_view(&base_path)?)
        } else {
            None
        };
        let delta = create_rw_index(dim, 1024)?;

        Ok(Self {
            dim,
            base_path,
            tombstone_path,
            flat,
            base: RwLock::new(base),
            delta: RwLock::new(delta),
            delta_ids: RwLock::new(RoaringBitmap::new()),
            tombstones: RwLock::new(tombstones),
        })
    }

    /// Vector dimensionality.
    pub const fn dim(&self) -> usize {
        self.dim
    }

    /// Reference to the underlying flat `f16` vector store.
    pub fn flat(&self) -> &Arc<FlatVectorStore> {
        &self.flat
    }

    /// Path to the `.usearch` base index file.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Returns `true` if a read-only mmap base `.usearch` index is currently loaded.
    pub fn has_base(&self) -> bool {
        self.base.read().is_some()
    }

    /// Number of vectors currently residing in the in-RAM delta index.
    pub fn delta_len(&self) -> usize {
        self.delta_ids.read().len() as usize
    }

    /// Number of active tombstoned vector IDs.
    pub fn tombstone_len(&self) -> u64 {
        self.tombstones.read().len()
    }

    /// L2-normalizes `vec_f32`, writes to `vectors.f16`, and inserts into the in-RAM delta index.
    pub fn add(&self, vector_id: u32, vec_f32: &[f32]) -> Result<(), AiError> {
        if vec_f32.len() != self.dim {
            return Err(AiError::InvalidInput(format!(
                "vector dim mismatch: expected {}, got {}",
                self.dim,
                vec_f32.len()
            )));
        }
        let norm = l2_normalize(vec_f32);
        self.flat.write_vector(vector_id, &norm)?;

        {
            let delta = self.delta.write();
            let mut delta_ids = self.delta_ids.write();
            if delta_ids.contains(vector_id) {
                let _ = delta.remove(vector_id as u64);
            }
            if delta.size() >= delta.capacity() {
                let next_cap = (delta.capacity().saturating_mul(2)).max(1024);
                delta
                    .reserve(next_cap)
                    .map_err(|e| AiError::VectorIndex(format!("delta reserve: {e}")))?;
            }
            delta
                .add(vector_id as u64, &norm)
                .map_err(|e| AiError::VectorIndex(format!("delta add: {e}")))?;
            delta_ids.insert(vector_id);
        }

        let mut tombstones = self.tombstones.write();
        if tombstones.remove(vector_id) {
            persist_tombstones(&self.tombstone_path, &tombstones)?;
        }

        Ok(())
    }

    /// Marks `vector_id` deleted in the persistent `.tombstones.roar` bitmap.
    pub fn mark_deleted(&self, vector_id: u32) -> Result<(), AiError> {
        {
            let mut tombstones = self.tombstones.write();
            if tombstones.insert(vector_id) {
                persist_tombstones(&self.tombstone_path, &tombstones)?;
            }
        }
        {
            let mut delta_ids = self.delta_ids.write();
            if delta_ids.remove(vector_id) {
                let delta = self.delta.write();
                let _ = delta.remove(vector_id as u64);
            }
        }
        Ok(())
    }

    /// Returns `true` when the delta index has reached `DELTA_COMPACT_THRESHOLD`.
    pub fn needs_compaction(&self) -> bool {
        self.delta_ids.read().len() as usize >= DELTA_COMPACT_THRESHOLD
    }

    /// Rebuilds the base `.usearch` index from all non-tombstoned rows in `vectors.f16`,
    /// atomically swaps it onto disk, re-opens via read-only `Index::view`, and clears delta.
    pub fn compact_and_swap(&self) -> Result<(), AiError> {
        self.rebuild_from_flat()
    }

    /// Reconstructs `.usearch` from `vectors.f16` (excluding tombstones) and opens it read-only via `view()`.
    pub fn rebuild_from_flat(&self) -> Result<(), AiError> {
        let count = self.flat.count();
        let tombstones_snapshot = self.tombstones.read().clone();

        let mut active_ids = Vec::new();
        for id_u64 in 0..count {
            let id = id_u64 as u32;
            if !tombstones_snapshot.contains(id) {
                active_ids.push(id);
            }
        }

        let rebuilt = create_rw_index(self.dim, active_ids.len().max(1))?;
        if active_ids.len() <= 10_000 {
            for &id in &active_ids {
                if let Some(vec_f32) = self.flat.get_f32(id) {
                    rebuilt
                        .add(id as u64, &vec_f32)
                        .map_err(|e| AiError::VectorIndex(format!("rebuild add {id}: {e}")))?;
                }
            }
        } else {
            active_ids
                .par_iter()
                .try_for_each(|&id| -> Result<(), AiError> {
                    if let Some(vec_f32) = self.flat.get_f32(id) {
                        rebuilt
                            .add(id as u64, &vec_f32)
                            .map_err(|e| AiError::VectorIndex(format!("rebuild add {id}: {e}")))?;
                    }
                    Ok(())
                })?;
        }

        let tmp_path = PathBuf::from(format!("{}.tmp", self.base_path.display()));
        let tmp_str = tmp_path
            .to_str()
            .ok_or_else(|| AiError::InvalidInput("non-UTF8 base index tmp path".to_string()))?;
        rebuilt
            .save(tmp_str)
            .map_err(|e| AiError::VectorIndex(format!("save tmp base index: {e}")))?;

        let new_delta = create_rw_index(self.dim, 1024)?;

        // Hold base write lock while dropping old mmap view, atomically renaming, and re-viewing.
        let mut base_guard = self.base.write();
        *base_guard = None;
        std::fs::rename(&tmp_path, &self.base_path)?;
        *base_guard = Some(open_base_view(&self.base_path)?);

        let mut delta_guard = self.delta.write();
        let mut delta_ids_guard = self.delta_ids.write();
        *delta_guard = new_delta;
        delta_ids_guard.clear();

        Ok(())
    }

    /// Exact parallel SIMD cosine scan over `allowed - tombstones`.
    pub fn search_exact(
        &self,
        query: &[f32],
        allowed: &RoaringBitmap,
        k: usize,
    ) -> Vec<(u32, f32)> {
        if k == 0 || query.len() != self.dim || allowed.is_empty() {
            return Vec::new();
        }
        let effective = {
            let tombstones = self.tombstones.read();
            allowed - &*tombstones
        };
        self.flat.exact_top_k(query, &effective, k)
    }

    /// HNSW filtered search over Base (read-only mmap view) + Delta (in-RAM RW), excluding tombstones.
    pub fn search_hnsw(
        &self,
        query: &[f32],
        allowed: &RoaringBitmap,
        k: usize,
    ) -> Result<Vec<(u32, f32)>, AiError> {
        if k == 0 || query.len() != self.dim || allowed.is_empty() {
            return Ok(Vec::new());
        }
        let norm_query = l2_normalize(query);
        let tombstones = self.tombstones.read();
        let effective = allowed - &*tombstones;
        drop(tombstones);
        if effective.is_empty() {
            return Ok(Vec::new());
        }

        let delta_ids = self.delta_ids.read().clone();
        let mut merged: HashMap<u32, f32> = HashMap::with_capacity(k * 2);

        {
            let base_guard = self.base.read();
            if let Some(base) = base_guard.as_ref() {
                if base.size() > 0 {
                    let matches = base
                        .filtered_search(&norm_query, k, |key| {
                            let id = key as u32;
                            effective.contains(id) && !delta_ids.contains(id)
                        })
                        .map_err(|e| AiError::VectorIndex(format!("base filtered_search: {e}")))?;
                    for (key, dist) in matches.keys.into_iter().zip(matches.distances.into_iter()) {
                        let sim = 1.0 - dist;
                        merged.insert(key as u32, sim);
                    }
                }
            }
        }

        {
            let delta_guard = self.delta.read();
            if delta_guard.size() > 0 {
                let matches = delta_guard
                    .filtered_search(&norm_query, k, |key| effective.contains(key as u32))
                    .map_err(|e| AiError::VectorIndex(format!("delta filtered_search: {e}")))?;
                for (key, dist) in matches.keys.into_iter().zip(matches.distances.into_iter()) {
                    let sim = 1.0 - dist;
                    merged.insert(key as u32, sim);
                }
            }
        }

        let mut results: Vec<(u32, f32)> = merged.into_iter().collect();
        top_k_sort(&mut results, k);
        Ok(results)
    }

    /// Hybrid query planner:
    /// - Uses `ExactSimd` when `(allowed - tombstones).len() <= EXACT_SCAN_THRESHOLD`
    /// - Uses `Hnsw` over Base + Delta when cardinality exceeds `EXACT_SCAN_THRESHOLD`
    pub fn search_hybrid(
        &self,
        query: &[f32],
        allowed: &RoaringBitmap,
        k: usize,
    ) -> Result<(Vec<(u32, f32)>, PlannerMode), AiError> {
        if k == 0 || query.len() != self.dim || allowed.is_empty() {
            return Ok((Vec::new(), PlannerMode::ExactSimd));
        }
        let effective = {
            let tombstones = self.tombstones.read();
            allowed - &*tombstones
        };
        if effective.is_empty() {
            return Ok((Vec::new(), PlannerMode::ExactSimd));
        }

        if effective.len() <= EXACT_SCAN_THRESHOLD {
            let hits = self.flat.exact_top_k(query, &effective, k);
            Ok((hits, PlannerMode::ExactSimd))
        } else {
            let hits = self.search_hnsw(query, allowed, k)?;
            Ok((hits, PlannerMode::Hnsw))
        }
    }
}

fn create_rw_index(dim: usize, capacity: usize) -> Result<Index, AiError> {
    let options = IndexOptions {
        dimensions: dim,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F16,
        connectivity: 16,
        expansion_add: 128,
        expansion_search: HNSW_EF_SEARCH,
        multi: false,
    };
    let index = Index::new(&options)
        .map_err(|e| AiError::VectorIndex(format!("usearch Index::new: {e}")))?;
    index
        .reserve(capacity.max(1))
        .map_err(|e| AiError::VectorIndex(format!("usearch reserve: {e}")))?;
    Ok(index)
}

fn open_base_view(path: &Path) -> Result<Index, AiError> {
    let path_str = path
        .to_str()
        .ok_or_else(|| AiError::InvalidInput("non-UTF8 base index path".to_string()))?;
    let meta = Index::metadata(path_str)
        .map_err(|e| AiError::VectorIndex(format!("usearch metadata {}: {e}", path.display())))?;
    let index = Index::new(&meta.into())
        .map_err(|e| AiError::VectorIndex(format!("usearch Index::new(meta): {e}")))?;
    index
        .view(path_str)
        .map_err(|e| AiError::VectorIndex(format!("usearch view {}: {e}", path.display())))?;
    index.change_expansion_search(HNSW_EF_SEARCH);
    Ok(index)
}

fn load_tombstones(path: &Path) -> Result<RoaringBitmap, AiError> {
    if !path.is_file() {
        return Ok(RoaringBitmap::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    RoaringBitmap::deserialize_from(reader).map_err(|e| {
        AiError::VectorIndex(format!("deserialize tombstones {}: {e}", path.display()))
    })
}

fn persist_tombstones(path: &Path, tombstones: &RoaringBitmap) -> Result<(), AiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        tombstones.serialize_into(&mut writer).map_err(|e| {
            AiError::VectorIndex(format!("serialize tombstones {}: {e}", path.display()))
        })?;
        use std::io::Write;
        writer.flush()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}
