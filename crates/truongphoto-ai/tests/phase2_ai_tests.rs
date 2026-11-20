use image::{Rgb, RgbImage};
use roaring::RoaringBitmap;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use truongphoto_ai::alignment::{
    align_face_112, estimate_similarity_transform, ARCFACE_REF_LANDMARKS,
};
use truongphoto_ai::clustering::{
    chinese_whispers_cluster, decode_centroid_le, encode_centroid_le, match_face_to_centroids,
    update_running_centroid, DEFAULT_CW_SIM_THRESHOLD, DEFAULT_FACE_MATCH_THRESHOLD,
};
use truongphoto_ai::faces::crop_face_thumbnail_webp;
use truongphoto_ai::models::{find_model_spec, ModelDownloader, ModelFileSpec, MODEL_MANIFEST};
use truongphoto_ai::ocr::{
    ctc_greedy_decode, parse_paddle_character_dict, select_rec_width_bucket,
};
use truongphoto_ai::ort_runtime::OrtRuntimeManager;
use truongphoto_ai::vector::{
    collapse_video_keyframes, l2_normalize, overfetch_k, FlatVectorStore, HybridVectorIndex,
    KeyframeHit, EXACT_SCAN_THRESHOLD,
};
use truongphoto_ai::{AiError, AiPipeline};
use truongphoto_core::{AiTier, PlannerMode};

struct TestRng(u64);

impl TestRng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn next_signed(&mut self) -> f32 {
        let u = (self.next_u64() & 0x00ff_ffff) as f32 / 16_777_216.0;
        u * 2.0 - 1.0
    }
}

fn random_unit_vec(rng: &mut TestRng, dim: usize) -> Vec<f32> {
    let raw: Vec<f32> = (0..dim).map(|_| rng.next_signed()).collect();
    l2_normalize(&raw)
}

fn jitter_unit_vec(base: &[f32], rng: &mut TestRng, scale: f32) -> Vec<f32> {
    let noisy: Vec<f32> = base
        .iter()
        .map(|&v| v + rng.next_signed() * scale)
        .collect();
    l2_normalize(&noisy)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

#[test]
fn exact_vs_hnsw_threshold_and_recall_with_tombstones() {
    let dir = tempdir().unwrap();
    let dim = 768;
    let index = HybridVectorIndex::open(dir.path(), "vision_test", dim).unwrap();

    let mut rng = TestRng(0xdead_beef_cafe_1234);
    let num_clusters = 50usize;
    let total_vecs = 5_000u32;

    let centers: Vec<Vec<f32>> = (0..num_clusters)
        .map(|_| random_unit_vec(&mut rng, dim))
        .collect();

    let mut all_vecs = Vec::with_capacity(total_vecs as usize);
    for id in 0..total_vecs {
        let c = &centers[(id as usize) % num_clusters];
        let v = jitter_unit_vec(c, &mut rng, 0.08);
        all_vecs.push(v);
    }

    // Put first 4,000 into Base via compact_and_swap, then 1,000 into Delta.
    for id in 0..4_000u32 {
        index.add(id, &all_vecs[id as usize]).unwrap();
    }
    index.compact_and_swap().unwrap();
    assert!(index.has_base());
    assert_eq!(index.delta_len(), 0);

    for id in 4_000..total_vecs {
        index.add(id, &all_vecs[id as usize]).unwrap();
    }
    assert_eq!(index.delta_len(), 1_000);

    // Verify FlatVectorStore chunk growth beyond 4,096 vectors.
    assert_eq!(index.flat().count(), total_vecs as u64);
    assert!(index.flat().get_f32(4_999).is_some());
    assert!(index.flat().get_f32(5_000).is_none());

    // 1. Narrow RoaringBitmap (<= 35,000) -> ExactSimd with 100.0% recall@10.
    let allowed_all: RoaringBitmap = (0..total_vecs).collect();
    assert!(allowed_all.len() <= EXACT_SCAN_THRESHOLD);

    let query = jitter_unit_vec(&centers[7], &mut rng, 0.02);
    let (exact_hits, mode) = index.search_hybrid(&query, &allowed_all, 10).unwrap();
    assert_eq!(mode, PlannerMode::ExactSimd);
    assert_eq!(exact_hits.len(), 10);

    // Ground truth over f16-quantized vectors (matching stored precision).
    let mut gt: Vec<(u32, f32)> = (0..total_vecs)
        .map(|id| {
            let stored = index.flat().get_f32(id).unwrap();
            (id, dot(&query, &stored))
        })
        .collect();
    gt.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let gt_top10: HashSet<u32> = gt.iter().take(10).map(|x| x.0).collect();
    let exact_top10: HashSet<u32> = exact_hits.iter().map(|x| x.0).collect();
    assert_eq!(
        exact_top10, gt_top10,
        "ExactSimd must achieve 100% recall@10"
    );

    // 2. Tombstone the top-2 ground truth hits (one from Base, one from Delta if present) and test forced HNSW.
    let mut tombstoned = HashSet::new();
    for &(id, _) in gt.iter().take(2) {
        index.mark_deleted(id).unwrap();
        tombstoned.insert(id);
    }
    // Also explicitly tombstone one Delta vector from cluster 7.
    let delta_c7 = (4_000..total_vecs)
        .find(|&id| (id as usize) % num_clusters == 7)
        .unwrap();
    index.mark_deleted(delta_c7).unwrap();
    tombstoned.insert(delta_c7);

    let mut gt_filtered: Vec<(u32, f32)> = (0..total_vecs)
        .filter(|id| !tombstoned.contains(id))
        .map(|id| (id, dot(&query, &index.flat().get_f32(id).unwrap())))
        .collect();
    gt_filtered.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let gt_filtered_top20: HashSet<u32> = gt_filtered.iter().take(20).map(|x| x.0).collect();

    let hnsw_hits = index.search_hnsw(&query, &allowed_all, 20).unwrap();
    assert_eq!(hnsw_hits.len(), 20);
    for (id, _) in &hnsw_hits {
        assert!(
            !tombstoned.contains(id),
            "tombstoned ID {id} leaked into HNSW results"
        );
    }
    let hnsw_ids: HashSet<u32> = hnsw_hits.iter().map(|x| x.0).collect();
    let recall_count = hnsw_ids.intersection(&gt_filtered_top20).count();
    let recall = recall_count as f32 / 20.0;
    assert!(
        recall >= 0.95,
        "expected HNSW recall@20 >= 0.95 over Base+Delta, got {recall:.3} ({recall_count}/20)"
    );

    // 3. Large bitmap (> 35,000) routes search_hybrid to PlannerMode::Hnsw.
    let wide_allowed: RoaringBitmap = (0..36_000u32).collect();
    let (hybrid_hnsw_hits, hybrid_mode) = index.search_hybrid(&query, &wide_allowed, 10).unwrap();
    assert_eq!(hybrid_mode, PlannerMode::Hnsw);
    assert_eq!(hybrid_hnsw_hits.len(), 10);
    for (id, _) in &hybrid_hnsw_hits {
        assert!(!tombstoned.contains(id));
    }
}

#[test]
fn usearch_rebuild_from_flat_and_view_reopen() {
    let dir = tempdir().unwrap();
    let dim = 256;
    let mut rng = TestRng(0x1122_3344_5566_7788);

    let vectors: Vec<Vec<f32>> = (0..120).map(|_| random_unit_vec(&mut rng, dim)).collect();
    let query = jitter_unit_vec(&vectors[42], &mut rng, 0.01);
    let allowed: RoaringBitmap = (0..120u32).collect();

    let before_hits = {
        let index = HybridVectorIndex::open(dir.path(), "rebuild_idx", dim).unwrap();
        for (id, v) in vectors.iter().enumerate() {
            index.add(id as u32, v).unwrap();
        }
        index.compact_and_swap().unwrap();
        assert!(index.base_path().is_file());
        index.search_hnsw(&query, &allowed, 5).unwrap()
    };

    // Delete `.usearch` file on disk while keeping `vectors.f16` intact.
    let usearch_path = dir.path().join("rebuild_idx.usearch");
    assert!(usearch_path.is_file());
    std::fs::remove_file(&usearch_path).unwrap();
    assert!(!usearch_path.is_file());

    // Reopen without `.usearch`, rebuild from `vectors.f16`, and verify identical hits.
    {
        let reopened = HybridVectorIndex::open(dir.path(), "rebuild_idx", dim).unwrap();
        assert!(!reopened.has_base());
        reopened.rebuild_from_flat().unwrap();
        assert!(usearch_path.is_file());
        assert!(reopened.has_base());

        let after_rebuild_hits = reopened.search_hnsw(&query, &allowed, 5).unwrap();
        assert_eq!(after_rebuild_hits[0].0, 42);
        assert_eq!(
            after_rebuild_hits.iter().map(|x| x.0).collect::<Vec<_>>(),
            before_hits.iter().map(|x| x.0).collect::<Vec<_>>()
        );
    }

    // Reopen again to verify RO `Index::view()` loads the rebuilt `.usearch` file cleanly.
    let viewed = HybridVectorIndex::open(dir.path(), "rebuild_idx", dim).unwrap();
    assert!(viewed.has_base());
    let viewed_hits = viewed.search_hnsw(&query, &allowed, 5).unwrap();
    assert_eq!(viewed_hits[0].0, 42);
    assert_eq!(
        viewed_hits.iter().map(|x| x.0).collect::<Vec<_>>(),
        before_hits.iter().map(|x| x.0).collect::<Vec<_>>()
    );
}

#[test]
fn video_keyframe_collapse_retains_highest_scoring_timestamp() {
    assert_eq!(overfetch_k(1), 40);
    assert_eq!(overfetch_k(20), 80);

    let hits = vec![
        KeyframeHit {
            vector_id: 1,
            asset_id: "asset_v1".to_string(),
            timestamp_ms: 0,
            score: 0.42,
        },
        KeyframeHit {
            vector_id: 2,
            asset_id: "asset_v1".to_string(),
            timestamp_ms: 12_000,
            score: 0.91,
        },
        KeyframeHit {
            vector_id: 3,
            asset_id: "asset_v1".to_string(),
            timestamp_ms: 25_000,
            score: 0.67,
        },
        KeyframeHit {
            vector_id: 4,
            asset_id: "asset_p1".to_string(),
            timestamp_ms: 0,
            score: 0.55,
        },
    ];

    let collapsed = collapse_video_keyframes(hits, 10);
    assert_eq!(collapsed.len(), 2);
    assert_eq!(collapsed[0].asset_id, "asset_v1");
    assert!((collapsed[0].score - 0.91).abs() < 1e-6);
    assert_eq!(collapsed[0].timestamp_sec, Some(12.0));

    assert_eq!(collapsed[1].asset_id, "asset_p1");
    assert_eq!(collapsed[1].timestamp_sec, None);
}

#[test]
fn chinese_whispers_clusters_three_pure_groups_and_excludes_outlier() {
    let mut rng = TestRng(0x9876_5432_10fe_dcba);
    let dim = 512;

    // Construct 3 mutually orthogonal cluster centers + 1 orthogonal outlier.
    let mut c0 = vec![0.0f32; dim];
    c0[0..128].fill(1.0);
    let c0 = l2_normalize(&c0);

    let mut c1 = vec![0.0f32; dim];
    c1[128..256].fill(1.0);
    let c1 = l2_normalize(&c1);

    let mut c2 = vec![0.0f32; dim];
    c2[256..384].fill(1.0);
    let c2 = l2_normalize(&c2);

    let mut outlier = vec![0.0f32; dim];
    outlier[384..512].fill(1.0);
    let outlier = l2_normalize(&outlier);

    let mut nodes: Vec<(u32, Vec<f32>)> = Vec::new();
    let mut true_group: HashMap<u32, usize> = HashMap::new();
    let mut next_id = 100u32;

    for (grp, center) in [&c0, &c1, &c2].iter().enumerate() {
        for _ in 0..5 {
            let v = jitter_unit_vec(center, &mut rng, 0.03);
            nodes.push((next_id, v));
            true_group.insert(next_id, grp);
            next_id += 1;
        }
    }
    let outlier_id = 999u32;
    nodes.push((outlier_id, outlier.clone()));

    let clusters = chinese_whispers_cluster(&nodes, DEFAULT_CW_SIM_THRESHOLD, 2, 42);
    assert_eq!(clusters.len(), 3, "expected exactly 3 pure clusters");

    let mut seen_groups = HashSet::new();
    for cluster in &clusters {
        assert_eq!(cluster.len(), 5);
        assert!(!cluster.contains(&outlier_id));
        let g = true_group[&cluster[0]];
        assert!(cluster.iter().all(|id| true_group[id] == g));
        seen_groups.insert(g);
    }
    assert_eq!(seen_groups.len(), 3);

    // Stage 1 incremental centroid matching + blob roundtrip.
    let centroids = vec![
        ("person-a".to_string(), c0.clone()),
        ("person-b".to_string(), c1.clone()),
    ];
    let probe = jitter_unit_vec(&c0, &mut rng, 0.02);
    let matched = match_face_to_centroids(&probe, &centroids, DEFAULT_FACE_MATCH_THRESHOLD);
    assert_eq!(matched.as_ref().map(|m| m.0.as_str()), Some("person-a"));
    assert!(match_face_to_centroids(&outlier, &centroids, DEFAULT_FACE_MATCH_THRESHOLD).is_none());

    let updated = update_running_centroid(&c0, 5, &probe);
    let blob = encode_centroid_le(&updated);
    assert_eq!(blob.len(), dim * 4);
    let decoded = decode_centroid_le(&blob).unwrap();
    assert_eq!(decoded, updated);
}

#[test]
fn umeyama_5point_alignment_and_ctc_greedy_decode() {
    // Apply a known scale + rotation + translation to ArcFace reference landmarks.
    let angle = 22.0_f32.to_radians();
    let scale = 1.35_f32;
    let tx = 48.0_f32;
    let ty = -16.0_f32;

    let mut src = [(0.0f32, 0.0f32); 5];
    for (i, &(rx, ry)) in ARCFACE_REF_LANDMARKS.iter().enumerate() {
        let sx = scale * (angle.cos() * rx - angle.sin() * ry) + tx;
        let sy = scale * (angle.sin() * rx + angle.cos() * ry) + ty;
        src[i] = (sx, sy);
    }

    let m = estimate_similarity_transform(&src, &ARCFACE_REF_LANDMARKS);
    for (i, &(sx, sy)) in src.iter().enumerate() {
        let px = m[0][0] * sx + m[0][1] * sy + m[0][2];
        let py = m[1][0] * sx + m[1][1] * sy + m[1][2];
        let err = ((px - ARCFACE_REF_LANDMARKS[i].0).powi(2)
            + (py - ARCFACE_REF_LANDMARKS[i].1).powi(2))
        .sqrt();
        assert!(err < 0.5, "landmark {i} alignment error {err} >= 0.5px");
    }

    // Exercise align_face_112 on a 256x256 synthetic image.
    let img = RgbImage::from_pixel(256, 256, Rgb([200, 150, 120]));
    let aligned = align_face_112(&img, &ARCFACE_REF_LANDMARKS);
    assert_eq!(aligned.dimensions(), (112, 112));
    assert_eq!(aligned.get_pixel(56, 56).0, [200, 150, 120]);

    // CTC greedy decode: [blank, 'H', 'H', blank, 'I', 'I'] -> "HI"
    let dict = vec!["H".to_string(), "I".to_string(), "!".to_string()];
    let num_classes = 4; // 0 = blank, 1 = 'H', 2 = 'I', 3 = '!'
    let sequence = [0usize, 1, 1, 0, 2, 2];
    let mut logits = vec![-10.0f32; sequence.len() * num_classes];
    for (t, &cls) in sequence.iter().enumerate() {
        logits[t * num_classes + cls] = 10.0;
    }
    let decoded = ctc_greedy_decode(&logits, sequence.len(), num_classes, &dict);
    assert_eq!(decoded, "HI");

    // PaddleOCR inference.yml character_dict parsing & width bucketing.
    let sample_yml = r#"
Global:
  model_name: PP-OCRv6_rec
PostProcess:
  name: CTCLabelDecode
  character_dict:
    - '0'
    - "1"
    - A
    - ''''
PreProcess:
  keep: true
"#;
    let parsed_dict = parse_paddle_character_dict(sample_yml);
    assert_eq!(parsed_dict, vec!["0", "1", "A", "'"]);

    assert_eq!(select_rec_width_bucket(120), 320);
    assert_eq!(select_rec_width_bucket(320), 320);
    assert_eq!(select_rec_width_bucket(321), 640);
    assert_eq!(select_rec_width_bucket(640), 640);
    assert_eq!(select_rec_width_bucket(641), 960);
    assert_eq!(select_rec_width_bucket(1200), 960);
}

async fn spawn_mock_http_server(payload: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/model.onnx");

    let handle = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut req_buf = [0u8; 1024];
            let _ = socket.read(&mut req_buf).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&payload).await;
            let _ = socket.shutdown().await;
        }
    });

    (url, handle)
}

#[tokio::test]
async fn model_downloader_sha256_success_and_mismatch_cleanup() {
    assert_eq!(MODEL_MANIFEST.len(), 12);
    for id in [
        "siglip2-base",
        "siglip2-large",
        "siglip2-so400m",
        "yunet",
        "auraface-v1",
        "pp-ocrv6-det",
        "pp-ocrv6-rec",
    ] {
        assert!(find_model_spec(id).is_some(), "missing manifest spec {id}");
    }

    let payload = b"synthetic-onnx-weights-payload-phase2-verified".repeat(64);
    let expected_sha = hex::encode(Sha256::digest(&payload));

    // 1. Valid SHA-256 download succeeds and persists verified file.
    let (ok_url, ok_srv) = spawn_mock_http_server(payload.clone()).await;
    let ok_url_static: &'static str = Box::leak(ok_url.into_boxed_str());
    let expected_sha_static: &'static str = Box::leak(expected_sha.clone().into_boxed_str());

    let ok_files = [ModelFileSpec {
        filename: "weights.onnx",
        url: ok_url_static,
        expected_sha256: Some(expected_sha_static),
        approx_bytes: payload.len() as u64,
    }];

    let tmp_ok = tempdir().unwrap();
    let downloader = ModelDownloader::new().unwrap();
    let mut progress_events = Vec::new();
    let manifest = downloader
        .download_files_into(
            tmp_ok.path(),
            &ok_files,
            CancellationToken::new(),
            |downloaded, total| progress_events.push((downloaded, total)),
        )
        .await
        .unwrap();
    ok_srv.await.unwrap();

    assert_eq!(manifest.get("weights.onnx"), Some(&expected_sha));
    assert!(!progress_events.is_empty());
    assert_eq!(progress_events.last().unwrap().0, payload.len() as u64);
    let final_file = tmp_ok.path().join("weights.onnx");
    assert!(final_file.is_file());
    assert!(!tmp_ok.path().join("weights.onnx.part").exists());
    assert_eq!(std::fs::read(&final_file).unwrap(), payload);

    // 2. Mismatched SHA-256 fails cleanly, removing `.part` and leaving zero corrupt final file.
    let (bad_url, bad_srv) = spawn_mock_http_server(payload.clone()).await;
    let bad_url_static: &'static str = Box::leak(bad_url.into_boxed_str());
    let bad_files = [ModelFileSpec {
        filename: "corrupt.onnx",
        url: bad_url_static,
        expected_sha256: Some("0000000000000000000000000000000000000000000000000000000000000000"),
        approx_bytes: payload.len() as u64,
    }];

    let tmp_bad = tempdir().unwrap();
    let err = downloader
        .download_files_into(
            tmp_bad.path(),
            &bad_files,
            CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap_err();
    bad_srv.await.unwrap();

    assert!(matches!(err, AiError::ChecksumMismatch { .. }));
    assert!(!tmp_bad.path().join("corrupt.onnx").exists());
    assert!(!tmp_bad.path().join("corrupt.onnx.part").exists());

    // 3. Pre-cancelled token exits cleanly without creating files.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let tmp_cancel = tempdir().unwrap();
    let cancel_err = downloader
        .download_files_into(tmp_cancel.path(), &ok_files, cancel, |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(cancel_err, AiError::Cancelled));
    assert!(!tmp_cancel.path().join("weights.onnx").exists());
}

#[test]
fn deterministic_ai_pipeline_and_ort_graceful_fallback() {
    // OrtRuntimeManager must never panic even if no ORT dylib is installed.
    let ort_mgr = Arc::new(OrtRuntimeManager::new(std::env::temp_dir()));
    let _available = ort_mgr.ort_available();
    assert!(!ort_mgr.ep_name().is_empty());

    let pipeline = AiPipeline::deterministic_test();

    // Warm orange sunset image vs blue ocean image.
    let sunset_img = RgbImage::from_pixel(128, 128, Rgb([245, 130, 30]));
    let ocean_img = RgbImage::from_pixel(128, 128, Rgb([20, 120, 230]));

    let sunset_vec = pipeline
        .vision
        .embed_image(AiTier::Tier2, &sunset_img)
        .unwrap();
    let ocean_vec = pipeline
        .vision
        .embed_image(AiTier::Tier2, &ocean_img)
        .unwrap();
    let query_vec = pipeline.vision.embed_text(AiTier::Tier2, "sunset").unwrap();

    assert_eq!(sunset_vec.len(), AiTier::Tier2.dim());
    assert!(dot(&sunset_vec, &query_vec) > dot(&ocean_vec, &query_vec));

    let tags = pipeline
        .vision
        .score_zero_shot_tags(AiTier::Tier2, &sunset_vec)
        .unwrap();
    assert!(tags.iter().any(|(t, _)| t == "sunset"));

    // Face detection & 160x160 WebP thumbnail crop.
    let faces = pipeline.faces.detect_and_embed(&sunset_img).unwrap();
    assert_eq!(faces.len(), 1);
    assert_eq!(faces[0].embedding.len(), 512);
    let thumb = crop_face_thumbnail_webp(
        &sunset_img,
        (
            faces[0].bbox_x,
            faces[0].bbox_y,
            faces[0].bbox_w,
            faces[0].bbox_h,
        ),
    )
    .unwrap();
    assert!(!thumb.is_empty());
    let decoded_thumb = image::load_from_memory(&thumb).unwrap();
    assert_eq!(decoded_thumb.width(), 160);
    assert_eq!(decoded_thumb.height(), 160);

    // Document OCR heuristic.
    let mut doc_img = RgbImage::from_pixel(128, 128, Rgb([250, 250, 250]));
    for x in 10..60 {
        doc_img.put_pixel(x, 20, Rgb([10, 10, 10]));
    }
    let ocr_res = pipeline.ocr.recognize(&doc_img).unwrap();
    assert!(ocr_res.full_text.contains("RECEIPT"));

    // FlatVectorStore direct persistence check.
    let dir = tempdir().unwrap();
    let flat_path = dir.path().join("direct.f16");
    let store = FlatVectorStore::open(&flat_path, 4).unwrap();
    store.write_vector(0, &[3.0, 4.0, 0.0, 0.0]).unwrap();
    let got = store.get_f32(0).unwrap();
    assert!((got[0] - 0.6).abs() < 1e-2);
    assert!((got[1] - 0.8).abs() < 1e-2);
}
