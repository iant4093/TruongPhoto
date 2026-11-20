use roaring::RoaringBitmap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use truongphoto_ai::{l2_normalize, HybridVectorIndex};
use truongphoto_core::{PlannerMode, RbacScope, Role};
use truongphoto_db::{repo, repo::assets::AssetListQuery, rusqlite::params, DbHandle};
use uuid::Uuid;

const DIM: usize = 768;
const NUM_CLUSTERS: usize = 67; // Coprime with 100 so every folder gets uniform cluster coverage

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[inline]
fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn next_unit_f32(seed: &mut u64) -> f32 {
    let bits = splitmix64(seed);
    let unit = ((bits >> 40) as f32) / ((1u32 << 24) as f32);
    unit * 2.0 - 1.0
}

fn generate_centroids() -> Vec<Vec<f32>> {
    let mut centroids = Vec::with_capacity(NUM_CLUSTERS);
    for c in 0..NUM_CLUSTERS {
        let mut seed = 0x5450_4345_4E54_0000u64 ^ (c as u64 + 1).wrapping_mul(0x9E37);
        let mut raw = Vec::with_capacity(DIM);
        for _ in 0..DIM {
            raw.push(next_unit_f32(&mut seed));
        }
        centroids.push(l2_normalize(&raw));
    }
    centroids
}

fn generate_clustered_vector(centroids: &[Vec<f32>], idx: usize) -> Vec<f32> {
    let cluster = &centroids[idx % centroids.len()];
    let mut seed = 0x4153_5345_5456_0000u64 ^ (idx as u64 + 1).wrapping_mul(0x517C_C1B7_2722_0A95);
    let mut raw = Vec::with_capacity(DIM);
    for &c_val in cluster {
        raw.push(c_val + 0.10 * next_unit_f32(&mut seed));
    }
    l2_normalize(&raw)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

async fn seed_folders_and_assets(
    db: &DbHandle,
    total_assets: usize,
    folder_count: usize,
    batch_size: usize,
) -> (Vec<String>, RoaringBitmap, RoaringBitmap) {
    let folder_ids: Vec<String> = (0..folder_count).map(|i| format!("folder-{i}")).collect();
    let base_ts: i64 = 1_672_531_200_000; // 2023-01-01T00:00:00Z

    let fids_clone = folder_ids.clone();
    db.write(move |conn| {
        let tx = conn.transaction()?;
        for (idx, fid) in fids_clone.iter().enumerate() {
            tx.execute(
                "INSERT INTO folders (id, name, root_path, is_managed_uploads, allow_disk_delete, is_offline, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, 0, 0, 0, ?4, ?4)",
                params![fid, format!("Folder {idx}"), format!("/synthetic/{fid}"), base_ts],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
    .expect("seed folders");

    let mut allowed_1pct = RoaringBitmap::new();
    let mut allowed_50pct = RoaringBitmap::new();

    for i in 0..total_assets {
        let mod100 = i % 100;
        if mod100 == 0 {
            allowed_1pct.insert(i as u32);
            allowed_50pct.insert(i as u32);
        } else if mod100 < 50 {
            allowed_50pct.insert(i as u32);
        }
    }

    let mut start = 0usize;
    while start < total_assets {
        let end = (start + batch_size).min(total_assets);
        let fids = folder_ids.clone();
        db.write(move |conn| {
            let tx = conn.transaction()?;
            {
                let mut asset_stmt = tx.prepare_cached(
                    "INSERT INTO assets (
                        id, folder_id, kind, capture_at_ms, width, height, aspect_ratio,
                        is_favorite, is_hidden, thumb_ready, vision_vector_id, ai_stage,
                        created_at_ms, updated_at_ms
                    ) VALUES (?1, ?2, 'photo', ?3, 4000, 3000, 1.333333, 0, 0, 1, ?4, 2, ?5, ?5)",
                )?;
                let mut geo_stmt = tx.prepare_cached(
                    "INSERT INTO asset_geo_index (
                        asset_id, folder_id, lat, lon, capture_at_ms, geo_city, geo_country
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;

                for i in start..end {
                    let mod100 = i % 100;
                    let folder_idx = if folder_count == 3 {
                        if mod100 == 0 {
                            0
                        } else if mod100 < 50 {
                            1
                        } else {
                            2
                        }
                    } else if mod100 == 0 {
                        0
                    } else if mod100 < 50 {
                        1
                    } else if mod100 < 70 {
                        2
                    } else if mod100 < 90 {
                        3
                    } else {
                        4
                    };

                    let fid = &fids[folder_idx];
                    let asset_id = format!("asset-{i:06}");
                    // Spread across ~36 months (3 years) for realistic month bucket aggregation
                    let capture_at_ms = base_ts + ((i as i64) * 189_216);
                    // Spread across global cities/coordinates inside a realistic bbox
                    let cluster_geo = i % 12;
                    let base_lat = -35.0 + (cluster_geo as f64) * 8.0;
                    let base_lon = -120.0 + (cluster_geo as f64) * 22.0;
                    let lat = base_lat + ((i % 97) as f64 - 48.0) * 0.01;
                    let lon = base_lon + (((i / 97) % 97) as f64 - 48.0) * 0.01;
                    let city = match cluster_geo % 4 {
                        0 => "Tokyo",
                        1 => "San Francisco",
                        2 => "Paris",
                        _ => "Sydney",
                    };
                    let country = match cluster_geo % 4 {
                        0 => "Japan",
                        1 => "United States",
                        2 => "France",
                        _ => "Australia",
                    };

                    asset_stmt.execute(params![asset_id, fid, capture_at_ms, i as i64, base_ts])?;
                    geo_stmt.execute(params![
                        asset_id,
                        fid,
                        lat,
                        lon,
                        capture_at_ms,
                        city,
                        country
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .expect("insert asset batch");

        start = end;
    }

    (folder_ids, allowed_1pct, allowed_50pct)
}

async fn run_concurrent_wal_soak(db: &DbHandle, total_assets: usize) -> usize {
    let busy_errors = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(24);

    // 16 concurrent readers
    for r in 0..16 {
        let db = db.clone();
        let busy = Arc::clone(&busy_errors);
        handles.push(tokio::spawn(async move {
            for iter in 0..20 {
                let res = match (r + iter) % 3 {
                    0 => {
                        db.read(|conn| {
                            repo::assets::list_assets(
                                conn,
                                &RbacScope::AdminAll,
                                &AssetListQuery {
                                    limit: 50,
                                    ..Default::default()
                                },
                            )
                            .map(|_| ())
                        })
                        .await
                    }
                    1 => {
                        db.read(|conn| {
                            repo::map::list_map_clusters(
                                conn,
                                &RbacScope::AdminAll,
                                -60.0,
                                75.0,
                                -150.0,
                                150.0,
                                4,
                            )
                            .map(|_| ())
                        })
                        .await
                    }
                    _ => {
                        db.read(|conn| {
                            repo::assets::count_active_assets(conn, &RbacScope::AdminAll)
                                .map(|_| ())
                        })
                        .await
                    }
                };
                if let Err(err) = res {
                    tracing::error!(reader = r, error = %err, "reader error during WAL soak");
                    busy.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    // 8 concurrent writers
    for w in 0..8 {
        let db = db.clone();
        let busy = Arc::clone(&busy_errors);
        handles.push(tokio::spawn(async move {
            for iter in 0..15 {
                let target_idx = (w * 997 + iter * 31) % total_assets;
                let aid = format!("asset-{target_idx:06}");
                let fav = (iter % 2) == 0;
                let res = db
                    .write_batched(move |tx| {
                        tx.execute(
                            "UPDATE assets SET is_favorite = ?1 WHERE id = ?2",
                            params![i64::from(fav), aid],
                        )?;
                        Ok(())
                    })
                    .await;
                if let Err(err) = res {
                    tracing::error!(writer = w, error = %err, "writer error during WAL soak");
                    busy.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.await.expect("wal soak task join");
    }

    busy_errors.load(Ordering::Relaxed)
}

fn recall_at_k(truth: &[(u32, f32)], approx: &[(u32, f32)], k: usize) -> f64 {
    let truth_ids: HashSet<u32> = truth.iter().take(k).map(|(id, _)| *id).collect();
    if truth_ids.is_empty() {
        return 1.0;
    }
    let hits = approx
        .iter()
        .take(k)
        .filter(|(id, _)| truth_ids.contains(id))
        .count();
    (hits as f64) / (truth_ids.len() as f64)
}

#[tokio::test]
async fn scale_regression_20k() {
    let tmp = TempDir::new("tp-scale-20k").expect("temp dir");
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");
    let total_assets = 20_000usize;

    let (folder_ids, allowed_1pct, allowed_50pct) =
        seed_folders_and_assets(&db, total_assets, 3, 5_000).await;
    assert_eq!(allowed_1pct.len(), 200);
    assert_eq!(allowed_50pct.len(), 10_000);

    // 1. Cursor pagination
    let (page1, cursor1, total_count) = db
        .read(|conn| {
            repo::assets::list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    limit: 100,
                    ..Default::default()
                },
            )
        })
        .await
        .expect("page1");
    assert_eq!(total_count, total_assets as i64);
    assert_eq!(page1.len(), 100);
    let cursor1 = cursor1.expect("cursor1");

    let (page2, _, _) = db
        .read(move |conn| {
            repo::assets::list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    cursor: Some(cursor1),
                    limit: 100,
                    ..Default::default()
                },
            )
        })
        .await
        .expect("page2");
    assert_eq!(page2.len(), 100);
    assert!(page1.last().unwrap().capture_at_ms >= page2.first().unwrap().capture_at_ms);

    // 2. Month buckets
    let buckets = db
        .read(|conn| repo::assets::list_timeline_buckets(conn, &RbacScope::AdminAll))
        .await
        .expect("timeline buckets");
    let sum_buckets: u64 = buckets.iter().map(|b| b.count).sum();
    assert_eq!(sum_buckets, total_assets as u64);

    // 3. Spatial clustering
    let rbac_50 = RbacScope::from_role_and_folders(
        Role::Viewer,
        vec![folder_ids[0].clone(), folder_ids[1].clone()],
    );
    let clusters = db
        .read(move |conn| {
            repo::map::list_map_clusters(conn, &rbac_50, -60.0, 75.0, -150.0, 150.0, 4)
        })
        .await
        .expect("map clusters");
    assert!(!clusters.clusters.is_empty());
    assert_eq!(clusters.total_in_view, 10_000);

    // 4. HybridVectorIndex: 20k clustered 768d f16 vectors (19.5k Base + 500 Delta + tombstones)
    let centroids = generate_centroids();
    let vec_dir = tmp.path().join("vectors");
    let index = HybridVectorIndex::open(&vec_dir, "vision_tier1", DIM).expect("open vector index");

    let base_count = 19_500usize;
    for i in 0..base_count {
        let vec = generate_clustered_vector(&centroids, i);
        index
            .flat()
            .write_vector(i as u32, &vec)
            .expect("write flat vector");
    }
    index.rebuild_from_flat().expect("rebuild base");

    for i in base_count..total_assets {
        let vec = generate_clustered_vector(&centroids, i);
        index.add(i as u32, &vec).expect("add delta vector");
    }

    // Mark 10 tombstones (including some in cluster 0)
    for tid in [
        0u32, 67, 134, 201, 1000, 1067, 19_500, 19_567, 19_634, 19_701,
    ] {
        index.mark_deleted(tid).expect("mark deleted");
    }

    let query = centroids[0].clone();

    // 1% Exact SIMD (100% recall@10 vs direct flat cosine scan)
    let (exact_hits, mode) = index
        .search_hybrid(&query, &allowed_1pct, 10)
        .expect("1% exact hybrid search");
    assert_eq!(mode, PlannerMode::ExactSimd);
    assert_eq!(exact_hits.len(), 10);

    let mut manual_1pct: Vec<(u32, f32)> = allowed_1pct
        .iter()
        .filter(|id| {
            ![
                0u32, 67, 134, 201, 1000, 1067, 19_500, 19_567, 19_634, 19_701,
            ]
            .contains(id)
        })
        .filter_map(|id| index.flat().get_f32(id).map(|v| (id, dot(&query, &v))))
        .collect();
    manual_1pct.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    manual_1pct.truncate(10);
    assert_eq!(recall_at_k(&manual_1pct, &exact_hits, 10), 1.0);

    // Forced HNSW (>= 95% recall@10)
    let truth_50pct = index.search_exact(&query, &allowed_50pct, 10);
    let hnsw_hits = index
        .search_hnsw(&query, &allowed_50pct, 10)
        .expect("forced hnsw search");
    assert_eq!(hnsw_hits.len(), 10);
    let hnsw_recall = recall_at_k(&truth_50pct, &hnsw_hits, 10);
    assert!(
        hnsw_recall >= 0.95,
        "expected >= 95% HNSW recall@10 on 20k, got {:.1}%",
        hnsw_recall * 100.0
    );

    // 5. 16-reader + 8-writer concurrent SQLite WAL soak -> 0 SQLITE_BUSY
    let busy_count = run_concurrent_wal_soak(&db, total_assets).await;
    assert_eq!(busy_count, 0, "expected 0 SQLITE_BUSY errors");

    db.shutdown().await.expect("db shutdown");
}

#[tokio::test]
#[ignore = "500k asset + 500k vector release soak benchmark (run with --release -- --ignored --nocapture)"]
async fn soak_500k() {
    let total_start = Instant::now();
    let tmp = TempDir::new("tp-soak-500k").expect("temp dir");
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");
    let total_assets = 500_000usize;

    let seed_start = Instant::now();
    let (folder_ids, allowed_1pct, allowed_50pct) =
        seed_folders_and_assets(&db, total_assets, 5, 10_000).await;
    assert_eq!(allowed_1pct.len(), 5_000);
    assert_eq!(allowed_50pct.len(), 250_000);
    println!(
        "[soak_500k] SQLite seeded 500,000 assets + 500,000 geo rows across 5 folders in {:.2?}",
        seed_start.elapsed()
    );

    // Build 500,000 clustered 768d unit vectors (499k Base + 1k Delta + tombstones)
    let vec_build_start = Instant::now();
    let centroids = generate_centroids();
    let vec_dir = tmp.path().join("vectors");
    let index = HybridVectorIndex::open(&vec_dir, "vision_tier1", DIM).expect("open vector index");

    let base_count = 499_000usize;
    for i in 0..base_count {
        let vec = generate_clustered_vector(&centroids, i);
        index
            .flat()
            .write_vector(i as u32, &vec)
            .expect("write flat vector");
    }
    let flat_elapsed = vec_build_start.elapsed();
    let rebuild_start = Instant::now();
    index.rebuild_from_flat().expect("rebuild 499k base");
    let rebuild_elapsed = rebuild_start.elapsed();

    for i in base_count..total_assets {
        let vec = generate_clustered_vector(&centroids, i);
        index.add(i as u32, &vec).expect("add 1k delta vector");
    }

    // Mark 50 tombstones across base and delta
    let mut tombstones = HashSet::new();
    for k in 0..40u32 {
        let vid = k * 67;
        tombstones.insert(vid);
        index.mark_deleted(vid).expect("mark base tombstone");
    }
    for k in 0..10u32 {
        let vid = 499_000 + k * 67;
        tombstones.insert(vid);
        index.mark_deleted(vid).expect("mark delta tombstone");
    }
    println!(
        "[soak_500k] Built 500,000 768d f16 vectors (flat write {:.2?}, HNSW rebuild {:.2?}, total {:.2?})",
        flat_elapsed,
        rebuild_elapsed,
        vec_build_start.elapsed()
    );

    // Warm page cache briefly so cold vs steady-state measurements reflect Section 5 SLOs
    let _ = db
        .read(|conn| repo::assets::count_active_assets(conn, &RbacScope::AdminAll))
        .await
        .expect("warmup");

    // SLO 1: Timeline page (limit = 100) < 50 ms
    let t0 = Instant::now();
    let (timeline_page, _next_cursor, _total) = db
        .read(|conn| {
            repo::assets::list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    limit: 100,
                    ..Default::default()
                },
            )
        })
        .await
        .expect("timeline page");
    let timeline_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(timeline_page.len(), 100);
    println!("[soak_500k] Timeline page (limit=100): {timeline_ms:.2} ms (SLO < 50 ms)");
    assert!(
        timeline_ms < 50.0,
        "Timeline page SLO failed: {timeline_ms:.2} ms >= 50 ms"
    );

    // SLO 2: Month buckets (list_timeline_buckets) < 500 ms
    let t1 = Instant::now();
    let buckets = db
        .read(|conn| repo::assets::list_timeline_buckets(conn, &RbacScope::AdminAll))
        .await
        .expect("timeline buckets");
    let buckets_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let bucket_sum: u64 = buckets.iter().map(|b| b.count).sum();
    assert_eq!(bucket_sum, total_assets as u64);
    println!("[soak_500k] Month buckets (500k rows): {buckets_ms:.2} ms (SLO < 500 ms)");
    assert!(
        buckets_ms < 500.0,
        "Month buckets SLO failed: {buckets_ms:.2} ms >= 500 ms"
    );

    // SLO 3: Spatial map viewport clustering (list_map_clusters) < 100 ms
    let rbac_50_scope = RbacScope::from_role_and_folders(
        Role::Viewer,
        vec![folder_ids[0].clone(), folder_ids[1].clone()],
    );
    let t2 = Instant::now();
    let clusters = db
        .read(move |conn| {
            repo::map::list_map_clusters(conn, &rbac_50_scope, -10.0, 25.0, -60.0, 60.0, 6)
        })
        .await
        .expect("map clusters");
    let map_ms = t2.elapsed().as_secs_f64() * 1000.0;
    assert!(!clusters.clusters.is_empty());
    println!(
        "[soak_500k] Spatial map clustering (50% RBAC viewport, {} clusters / {} points): {map_ms:.2} ms (SLO < 100 ms)",
        clusters.clusters.len(),
        clusters.total_in_view
    );
    assert!(
        map_ms < 100.0,
        "Map clustering SLO failed: {map_ms:.2} ms >= 100 ms"
    );

    // SLO 4: Narrow 1% RBAC semantic query (5,000 allowed -> ExactSimd) < 15 ms, 100.0% recall@10
    let query = centroids[0].clone();
    let t3 = Instant::now();
    let (hits_1pct, mode_1pct) = index
        .search_hybrid(&query, &allowed_1pct, 10)
        .expect("1% semantic query");
    let exact_ms = t3.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(mode_1pct, PlannerMode::ExactSimd);
    assert_eq!(hits_1pct.len(), 10);

    let mut truth_1pct: Vec<(u32, f32)> = allowed_1pct
        .iter()
        .filter(|id| !tombstones.contains(id))
        .filter_map(|id| index.flat().get_f32(id).map(|v| (id, dot(&query, &v))))
        .collect();
    truth_1pct.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    truth_1pct.truncate(10);
    let recall_1pct = recall_at_k(&truth_1pct, &hits_1pct, 10);
    println!(
        "[soak_500k] 1% RBAC Exact SIMD (5,000 allowed): {exact_ms:.2} ms (SLO < 15 ms), recall@10 = {:.1}%",
        recall_1pct * 100.0
    );
    assert!(
        exact_ms < 15.0,
        "1% Exact SIMD SLO failed: {exact_ms:.2} ms >= 15 ms"
    );
    assert_eq!(recall_1pct, 1.0, "1% Exact SIMD must have 100% recall@10");

    // SLO 5: Broad 50% RBAC semantic query (250,000 allowed -> USearch HNSW ef=256) < 30 ms, >= 95% recall@10
    let truth_50pct = index.search_exact(&query, &allowed_50pct, 10);
    let t4 = Instant::now();
    let (hits_50pct, mode_50pct) = index
        .search_hybrid(&query, &allowed_50pct, 10)
        .expect("50% semantic query");
    let hnsw_ms = t4.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(mode_50pct, PlannerMode::Hnsw);
    assert_eq!(hits_50pct.len(), 10);
    let recall_50pct = recall_at_k(&truth_50pct, &hits_50pct, 10);
    println!(
        "[soak_500k] 50% RBAC HNSW ef=256 (250,000 allowed): {hnsw_ms:.2} ms (SLO < 30 ms), recall@10 = {:.1}% (SLO >= 95%)",
        recall_50pct * 100.0
    );
    assert!(
        hnsw_ms < 30.0,
        "50% HNSW SLO failed: {hnsw_ms:.2} ms >= 30 ms"
    );
    assert!(
        recall_50pct >= 0.95,
        "50% HNSW recall@10 SLO failed: {:.1}% < 95%",
        recall_50pct * 100.0
    );

    // SLO 6: 16 concurrent readers + 8 concurrent writers soak -> 0 SQLITE_BUSY errors
    let t5 = Instant::now();
    let busy_errors = run_concurrent_wal_soak(&db, total_assets).await;
    println!(
        "[soak_500k] 16-reader + 8-writer WAL soak completed in {:.2?} (SQLITE_BUSY errors = {busy_errors})",
        t5.elapsed()
    );
    assert_eq!(busy_errors, 0, "expected 0 SQLITE_BUSY errors");

    db.shutdown().await.expect("db shutdown");
    println!(
        "[soak_500k] ALL Section 5 SLOs PASSED in {:.2?} total!",
        total_start.elapsed()
    );
}
