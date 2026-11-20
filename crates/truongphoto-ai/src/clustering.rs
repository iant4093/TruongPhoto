use crate::vector::l2_normalize;
use std::collections::HashMap;

/// Default Stage 1 cosine similarity threshold for matching a face to existing named people.
pub const DEFAULT_FACE_MATCH_THRESHOLD: f32 = 0.52;

/// Default Stage 2 Chinese Whispers edge similarity threshold for clustering unassigned faces.
pub const DEFAULT_CW_SIM_THRESHOLD: f32 = 0.55;

#[inline]
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let xf = x as f64;
        let yf = y as f64;
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    let denom = (na * nb).sqrt();
    if denom <= 1e-12 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

/// Stage 1 incremental match: compares `embedding` against existing person centroids
/// and returns the highest-scoring `(person_id, similarity)` meeting `threshold`.
pub fn match_face_to_centroids(
    embedding: &[f32],
    centroids: &[(String, Vec<f32>)],
    threshold: f32,
) -> Option<(String, f32)> {
    let mut best: Option<(String, f32)> = None;
    for (person_id, centroid) in centroids {
        let sim = cosine_similarity(embedding, centroid);
        if sim >= threshold {
            let replace = match &best {
                Some((best_id, best_sim)) => {
                    sim > *best_sim || (sim == *best_sim && person_id < best_id)
                }
                None => true,
            };
            if replace {
                best = Some((person_id.clone(), sim));
            }
        }
    }
    best
}

/// Computes the updated L2-normalized running centroid after adding `new_emb`
/// to a centroid that currently averages `count` embeddings.
pub fn update_running_centroid(centroid: &[f32], count: u64, new_emb: &[f32]) -> Vec<f32> {
    if centroid.is_empty() || count == 0 {
        return l2_normalize(new_emb);
    }
    if centroid.len() != new_emb.len() {
        return l2_normalize(new_emb);
    }
    let c = count as f32;
    let denom = c + 1.0;
    let avg: Vec<f32> = centroid
        .iter()
        .zip(new_emb.iter())
        .map(|(&old, &new)| (old * c + new) / denom)
        .collect();
    l2_normalize(&avg)
}

/// Encodes a float32 centroid slice as little-endian bytes (`person_centroids.centroid_blob`).
pub fn encode_centroid_le(centroid: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(centroid.len() * 4);
    for &val in centroid {
        out.extend_from_slice(&val.to_le_bytes());
    }
    out
}

/// Decodes a little-endian float32 centroid blob, returning `None` if malformed or empty.
pub fn decode_centroid_le(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            (self.next_u64() % (upper as u64)) as usize
        }
    }
}

/// Stage 2 batch clustering: builds a sparse `k = 32` undirected cosine similarity graph
/// (`>= sim_threshold`) and runs 20 deterministic-shuffled Chinese Whispers iterations.
///
/// Returns clusters of original node IDs with `cluster.len() >= min_cluster_size`.
pub fn chinese_whispers_cluster(
    nodes: &[(u32, Vec<f32>)],
    sim_threshold: f32,
    min_cluster_size: usize,
    seed: u64,
) -> Vec<Vec<u32>> {
    let n = nodes.len();
    if n == 0 || min_cluster_size == 0 {
        return Vec::new();
    }

    const KNN_K: usize = 32;
    let norm_vecs: Vec<Vec<f32>> = nodes.iter().map(|(_, v)| l2_normalize(v)).collect();

    // Build top-K neighbors per node above sim_threshold, then symmetrize into undirected edges.
    let mut adj_maps: Vec<HashMap<usize, f32>> = vec![HashMap::new(); n];
    for i in 0..n {
        let mut candidates: Vec<(usize, f32)> = Vec::new();
        for j in 0..n {
            if i == j {
                continue;
            }
            let sim: f32 = norm_vecs[i]
                .iter()
                .zip(norm_vecs[j].iter())
                .map(|(&a, &b)| a * b)
                .sum();
            if sim >= sim_threshold {
                candidates.push((j, sim));
            }
        }
        candidates.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidates.truncate(KNN_K);
        for (j, sim) in candidates {
            adj_maps[i].insert(j, sim);
            adj_maps[j].insert(i, sim);
        }
    }

    let adj: Vec<Vec<(usize, f32)>> = adj_maps
        .into_iter()
        .map(|m| {
            let mut v: Vec<(usize, f32)> = m.into_iter().collect();
            v.sort_unstable_by_key(|&(nbr, _)| nbr);
            v
        })
        .collect();

    let mut labels: Vec<usize> = (0..n).collect();
    let mut order: Vec<usize> = (0..n).collect();
    let mut rng = SplitMix64(seed);

    for _iter in 0..20 {
        // Fisher-Yates shuffle
        for i in (1..n).rev() {
            let j = rng.gen_range(i + 1);
            order.swap(i, j);
        }

        let mut changed = false;
        for &u in &order {
            if adj[u].is_empty() {
                continue;
            }
            let mut weights: HashMap<usize, f32> = HashMap::new();
            for &(v, w) in &adj[u] {
                *weights.entry(labels[v]).or_insert(0.0) += w;
            }
            let mut best_label = labels[u];
            let mut best_weight = f32::NEG_INFINITY;
            for (lbl, w) in weights {
                if w > best_weight || (w == best_weight && lbl < best_label) {
                    best_weight = w;
                    best_label = lbl;
                }
            }
            if best_label != labels[u] {
                labels[u] = best_label;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut groups: HashMap<usize, Vec<u32>> = HashMap::new();
    for (idx, &lbl) in labels.iter().enumerate() {
        groups.entry(lbl).or_default().push(nodes[idx].0);
    }

    let mut clusters: Vec<Vec<u32>> = groups
        .into_values()
        .filter(|c| c.len() >= min_cluster_size)
        .map(|mut c| {
            c.sort_unstable();
            c
        })
        .collect();

    clusters.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    clusters
}
