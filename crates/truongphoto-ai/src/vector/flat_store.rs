use crate::AiError;
use half::f16;
use half::slice::HalfFloatSliceExt;
use memmap2::Mmap;
use parking_lot::RwLock;
use rayon::prelude::*;
use roaring::RoaringBitmap;
use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 8-byte magic header for `vectors.f16` (`TPVEC16\0`).
pub const FLAT_MAGIC: [u8; 8] = *b"TPVEC16\0";

/// 32-byte aligned header length.
pub const FLAT_HEADER_BYTES: usize = 32;

/// Pre-allocation chunk size in vectors (reduces remap frequency).
pub const CHUNK_VECTORS: usize = 4096;

/// L2-normalizes a float32 slice into a new `Vec<f32>`.
pub fn l2_normalize(vec: &[f32]) -> Vec<f32> {
    let mut norm_sq: f64 = 0.0;
    for &x in vec {
        let xf = x as f64;
        norm_sq += xf * xf;
    }
    let norm = norm_sq.sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        return vec![0.0; vec.len()];
    }
    let inv = (1.0 / norm) as f32;
    vec.iter().map(|&x| x * inv).collect()
}

#[inline]
fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    let mut sum0 = 0.0f32;
    let mut sum1 = 0.0f32;
    let mut sum2 = 0.0f32;
    let mut sum3 = 0.0f32;

    let mut i = 0;
    let len = a.len().min(b.len());
    while i + 4 <= len {
        sum0 += a[i] * b[i];
        sum1 += a[i + 1] * b[i + 1];
        sum2 += a[i + 2] * b[i + 2];
        sum3 += a[i + 3] * b[i + 3];
        i += 4;
    }
    let mut tail = 0.0f32;
    while i < len {
        tail += a[i] * b[i];
        i += 1;
    }
    (sum0 + sum1) + (sum2 + sum3) + tail
}

struct FlatMmapInner {
    file: File,
    mmap: Option<Mmap>,
    count: u64,
    allocated_bytes: u64,
}

/// Append/update-capable memory-mapped `f16` flat vector store (`vectors.f16`).
pub struct FlatVectorStore {
    path: PathBuf,
    dim: usize,
    inner: RwLock<FlatMmapInner>,
}

impl std::fmt::Debug for FlatVectorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlatVectorStore")
            .field("path", &self.path)
            .field("dim", &self.dim)
            .field("count", &self.count())
            .finish()
    }
}

impl FlatVectorStore {
    /// Opens or creates a `FlatVectorStore` at `path` for fixed dimension `dim`.
    pub fn open(path: &Path, dim: usize) -> Result<Self, AiError> {
        if dim == 0 {
            return Err(AiError::InvalidInput(
                "vector dimension must be > 0".to_string(),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let existing_len = file.metadata()?.len();
        let row_bytes = (dim as u64) * 2;
        let chunk_bytes = (CHUNK_VECTORS as u64) * row_bytes;

        let (count, allocated_bytes) = if existing_len == 0 {
            let initial_bytes = (FLAT_HEADER_BYTES as u64) + chunk_bytes;
            file.set_len(initial_bytes)?;
            write_header(&mut file, dim as u32, 0, 0)?;
            file.flush()?;
            (0, initial_bytes)
        } else {
            if existing_len < FLAT_HEADER_BYTES as u64 {
                return Err(AiError::VectorIndex(format!(
                    "corrupt flat vector header in {} ({existing_len} bytes)",
                    path.display()
                )));
            }
            let (file_dim, file_count) = read_header(&mut file)?;
            if file_dim as usize != dim {
                return Err(AiError::VectorIndex(format!(
                    "dimension mismatch in {}: expected {dim}, found {file_dim}",
                    path.display()
                )));
            }
            let min_needed = (FLAT_HEADER_BYTES as u64) + file_count * row_bytes;
            let allocated = if existing_len < min_needed {
                let chunks = file_count.max(1).div_ceil(CHUNK_VECTORS as u64);
                let target = (FLAT_HEADER_BYTES as u64) + chunks * chunk_bytes;
                file.set_len(target)?;
                target
            } else {
                existing_len
            };
            (file_count, allocated)
        };

        let mmap = unsafe { Mmap::map(&file)? };

        Ok(Self {
            path: path.to_path_buf(),
            dim,
            inner: RwLock::new(FlatMmapInner {
                file,
                mmap: Some(mmap),
                count,
                allocated_bytes,
            }),
        })
    }

    /// Vector dimensionality.
    pub const fn dim(&self) -> usize {
        self.dim
    }

    /// Path to the backing `.f16` file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Logical slot count (`max_written_vector_id + 1`).
    pub fn count(&self) -> u64 {
        self.inner.read().count
    }

    /// L2-normalizes `vec_f32`, converts to IEEE-754 `f16`, and writes at slot `vector_id`.
    pub fn write_vector(&self, vector_id: u32, vec_f32: &[f32]) -> Result<(), AiError> {
        if vec_f32.len() != self.dim {
            return Err(AiError::InvalidInput(format!(
                "vector dim mismatch: expected {}, got {}",
                self.dim,
                vec_f32.len()
            )));
        }

        let norm_f32 = l2_normalize(vec_f32);
        let mut f16_buf = vec![f16::ZERO; self.dim];
        f16_buf.convert_from_f32_slice(&norm_f32);
        let payload_bytes: &[u8] = bytemuck::cast_slice(&f16_buf);

        let slot = vector_id as u64;
        let row_bytes = (self.dim as u64) * 2;
        let offset = (FLAT_HEADER_BYTES as u64) + slot * row_bytes;
        let required_end = offset + row_bytes;

        let mut guard = self.inner.write();
        if required_end > guard.allocated_bytes {
            let needed_slots = slot + 1;
            let chunks = needed_slots.div_ceil(CHUNK_VECTORS as u64);
            let new_len = (FLAT_HEADER_BYTES as u64) + chunks * (CHUNK_VECTORS as u64) * row_bytes;
            // Drop mmap before resizing so Windows and POSIX remaps remain clean.
            guard.mmap = None;
            guard.file.set_len(new_len)?;
            guard.allocated_bytes = new_len;
        }

        guard.file.seek(SeekFrom::Start(offset))?;
        guard.file.write_all(payload_bytes)?;

        let new_count = guard.count.max(slot + 1);
        if new_count != guard.count {
            guard.count = new_count;
            write_header_count(&mut guard.file, new_count)?;
        }
        guard.file.flush()?;

        if guard.mmap.is_none() {
            guard.mmap = Some(unsafe { Mmap::map(&guard.file)? });
        }

        Ok(())
    }

    /// Reads and decodes slot `vector_id` as `Vec<f32>`, returning `None` if `vector_id >= count`.
    pub fn get_f32(&self, vector_id: u32) -> Option<Vec<f32>> {
        let guard = self.inner.read();
        let slot = vector_id as u64;
        if slot >= guard.count {
            return None;
        }
        let mmap = guard.mmap.as_ref()?;
        let row_bytes = self.dim * 2;
        let start = FLAT_HEADER_BYTES + (slot as usize) * row_bytes;
        let end = start + row_bytes;
        if end > mmap.len() {
            return None;
        }
        let slice_f16 = bytes_to_f16_slice(&mmap[start..end]);
        Some(slice_f16.to_f32_vec())
    }

    /// Parallel SIMD exact cosine dot-product scan over `allowed` vector IDs.
    ///
    /// Returns up to `k` `(vector_id, cosine_similarity)` pairs sorted descending by score.
    pub fn exact_top_k(
        &self,
        query_f32: &[f32],
        allowed: &RoaringBitmap,
        k: usize,
    ) -> Vec<(u32, f32)> {
        if k == 0 || query_f32.len() != self.dim || allowed.is_empty() {
            return Vec::new();
        }
        let norm_query = l2_normalize(query_f32);
        let guard = self.inner.read();
        let count = guard.count;
        if count == 0 {
            return Vec::new();
        }
        let Some(mmap) = guard.mmap.as_ref() else {
            return Vec::new();
        };

        let ids: Vec<u32> = allowed
            .iter()
            .take_while(|&id| (id as u64) < count)
            .collect();
        if ids.is_empty() {
            return Vec::new();
        }

        let dim = self.dim;
        let row_bytes = dim * 2;
        let mmap_bytes: &[u8] = mmap;

        let mut scored: Vec<(u32, f32)> = ids
            .par_chunks(512)
            .flat_map_iter(|chunk| {
                let mut scratch = vec![0.0f32; dim];
                let mut local = Vec::with_capacity(chunk.len());
                for &id in chunk {
                    let start = FLAT_HEADER_BYTES + (id as usize) * row_bytes;
                    let end = start + row_bytes;
                    if end <= mmap_bytes.len() {
                        let row_bytes_slice = &mmap_bytes[start..end];
                        let f16_slice = bytes_to_f16_slice(row_bytes_slice);
                        f16_slice.convert_to_f32_slice(&mut scratch);
                        let score = dot_f32(&norm_query, &scratch);
                        local.push((id, score));
                    }
                }
                local
            })
            .collect();

        top_k_sort(&mut scored, k);
        scored
    }
}

#[inline]
fn bytes_to_f16_slice(bytes: &[u8]) -> &[f16] {
    bytemuck::cast_slice(bytes)
}

pub(crate) fn top_k_sort(scored: &mut Vec<(u32, f32)>, k: usize) {
    if k == 0 {
        scored.clear();
        return;
    }
    let cmp = |a: &(u32, f32), b: &(u32, f32)| -> Ordering {
        b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))
    };
    if scored.len() > k {
        scored.select_nth_unstable_by(k, cmp);
        scored.truncate(k);
    }
    scored.sort_unstable_by(cmp);
}

fn write_header(file: &mut File, dim: u32, count: u64, flags: u64) -> Result<(), AiError> {
    let mut header = [0u8; FLAT_HEADER_BYTES];
    header[0..8].copy_from_slice(&FLAT_MAGIC);
    header[8..12].copy_from_slice(&dim.to_le_bytes());
    header[12..16].copy_from_slice(&0u32.to_le_bytes());
    header[16..24].copy_from_slice(&count.to_le_bytes());
    header[24..32].copy_from_slice(&flags.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    Ok(())
}

fn write_header_count(file: &mut File, count: u64) -> Result<(), AiError> {
    file.seek(SeekFrom::Start(16))?;
    file.write_all(&count.to_le_bytes())?;
    Ok(())
}

fn read_header(file: &mut File) -> Result<(u32, u64), AiError> {
    let mut header = [0u8; FLAT_HEADER_BYTES];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    if header[0..8] != FLAT_MAGIC {
        return Err(AiError::VectorIndex(
            "invalid flat vector store magic header".to_string(),
        ));
    }
    let dim = u32::from_le_bytes(header[8..12].try_into().expect("4 bytes"));
    let count = u64::from_le_bytes(header[16..24].try_into().expect("8 bytes"));
    Ok((dim, count))
}
