use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use truongphoto_core::models::{
    AiModelStatus, AiTier, AssetKind, SearchMode, SearchRequest, TagSource, TagState,
    UpdatePersonRequest,
};
use truongphoto_core::rbac::RbacScope;
use truongphoto_db::repo;
use truongphoto_db::repo::ai::AiModelRow;
use truongphoto_db::repo::folders::CreateFolder;
use truongphoto_db::DbHandle;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tp-db-p2-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn seed_folder(conn: &Connection, id: &str, name: &str, root: &str) {
    repo::folders::create_folder(
        conn,
        &CreateFolder {
            id: id.to_string(),
            name: name.to_string(),
            root_path: root.to_string(),
            volume_uuid: None,
            volume_label: None,
            is_managed_uploads: false,
            allow_disk_delete: false,
        },
    )
    .expect("create folder");
}

fn seed_asset_with_file(
    conn: &Connection,
    asset_id: &str,
    folder_id: &str,
    kind: AssetKind,
    capture_at_ms: i64,
    is_favorite: bool,
    rel_path: &str,
) {
    let file_id = format!("file-{asset_id}");
    conn.execute(
        "INSERT INTO assets (
            id, folder_id, kind, primary_file_id, capture_at_ms, is_favorite, is_hidden,
            thumb_ready, ai_stage, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 1, 1, 1000, 1000)",
        params![
            asset_id,
            folder_id,
            kind.as_str(),
            file_id,
            capture_at_ms,
            i64::from(is_favorite),
        ],
    )
    .expect("insert asset");

    conn.execute(
        "INSERT INTO asset_files (
            id, asset_id, folder_id, rel_path, file_role, mime_type, extension,
            file_size, mtime_ms, partial_blake3, is_missing, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 'primary', 'image/jpeg', 'jpg', 1024, ?5, ?6, 0, 1000, 1000)",
        params![
            file_id,
            asset_id,
            folder_id,
            rel_path,
            capture_at_ms,
            format!("b3-{asset_id}"),
        ],
    )
    .expect("insert asset_file");
}

#[tokio::test]
async fn migration_0003_idempotency_and_ai_index_state_defaults() {
    let tmp = TempDir::new();
    let db_path = tmp.path().join("library.db");

    let db1 = DbHandle::open(&db_path).expect("open db");
    let (versions, state) = db1
        .read(|conn| {
            let mut stmt =
                conn.prepare("SELECT version, name FROM schema_migrations ORDER BY version ASC")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let state = repo::ai::get_ai_index_state(conn)?;
            Ok((rows, state))
        })
        .await
        .expect("query migrations and ai state");

    assert_eq!(
        versions,
        vec![
            (1, "0001_initial".to_string()),
            (2, "0002_phase1_media".to_string()),
            (3, "0003_phase2_ai".to_string()),
            (4, "0004_phase3_chat_map".to_string()),
            (5, "0005_r5_perf_indexes".to_string()),
        ]
    );
    assert_eq!(state.active_vision_tier, AiTier::Tier2);
    assert_eq!(state.staging_vision_tier, None);
    assert!(!state.paused);
    assert_eq!(state.idle_unload_secs, 300);
    db1.shutdown().await.expect("shutdown db1");

    // Reopen idempotently and test ai_models CRUD + ai_index_state mutations.
    let db2 = DbHandle::open(&db_path).expect("reopen db");
    db2.write(|conn| {
        let model = AiModelRow {
            model_id: "siglip2-large".to_string(),
            status: AiModelStatus::Installed,
            local_dir: Some("/tmp/models/siglip2-large".to_string()),
            bytes_total: 1_200_000,
            bytes_downloaded: 1_200_000,
            sha256_manifest: Some(r#"{"vision_model_fp16.onnx":"abc"}"#.to_string()),
            error_message: None,
            updated_at_ms: 5000,
        };
        repo::ai::upsert_ai_model_row(conn, &model)?;
        assert_eq!(
            repo::ai::get_ai_model_row(conn, "siglip2-large")?.as_ref(),
            Some(&model)
        );
        assert_eq!(repo::ai::list_ai_model_rows(conn)?.len(), 1);
        assert!(repo::ai::delete_ai_model_row(conn, "siglip2-large")?);
        assert!(repo::ai::get_ai_model_row(conn, "siglip2-large")?.is_none());

        let paused = repo::ai::set_ai_paused(conn, true, 6000)?;
        assert!(paused.paused);
        let updated_idle = repo::ai::update_ai_idle_unload_secs(conn, 900, 7000)?;
        assert_eq!(updated_idle.idle_unload_secs, 900);
        Ok(())
    })
    .await
    .expect("ai model and state crud");
    db2.shutdown().await.expect("shutdown db2");
}

#[tokio::test]
async fn vision_embeddings_dual_tier_isolation_and_cutover() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(move |conn| {
        seed_folder(conn, "f1", "Photos", "/tmp/f1");
        seed_asset_with_file(
            conn,
            "a-photo",
            "f1",
            AssetKind::Photo,
            1000,
            false,
            "p.jpg",
        );
        seed_asset_with_file(
            conn,
            "a-video",
            "f1",
            AssetKind::Video,
            2000,
            false,
            "v.mp4",
        );

        // Index both assets in Tier 2 (including 2 video keyframes).
        let v0 = repo::ai::allocate_next_vision_vector_id(conn, AiTier::Tier2)?;
        assert_eq!(v0, 0);
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, v0, "a-photo", 0, 100)?;
        repo::ai::mark_asset_ai_stage(conn, "a-photo", 2, 100)?;

        let v1 = repo::ai::allocate_next_vision_vector_id(conn, AiTier::Tier2)?;
        let v2 = v1 + 1;
        assert_eq!(v1, 1);
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, v1, "a-video", 0, 101)?;
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, v2, "a-video", 12_000, 101)?;
        repo::ai::mark_asset_ai_stage(conn, "a-video", 2, 101)?;

        let stats = repo::ai::count_ai_queue_stats(conn, AiTier::Tier2, None)?;
        assert_eq!(stats.indexed_assets, 2);
        assert_eq!(stats.pending_assets, 0);
        assert_eq!(stats.staging_remaining, 0);

        // Start staging Tier 3 while Tier 2 remains active.
        let staging_state = repo::ai::set_staging_vision_tier(conn, Some(AiTier::Tier3), 200)?;
        assert_eq!(staging_state.active_vision_tier, AiTier::Tier2);
        assert_eq!(staging_state.staging_vision_tier, Some(AiTier::Tier3));

        let needing_t3 = repo::ai::list_assets_needing_staging_tier(conn, AiTier::Tier3, 10)?;
        assert_eq!(
            needing_t3,
            vec!["a-video".to_string(), "a-photo".to_string()]
        );

        // Populate Tier 3 partially; Tier 2 rows must remain untouched.
        let t3_v0 = repo::ai::allocate_next_vision_vector_id(conn, AiTier::Tier3)?;
        assert_eq!(t3_v0, 0);
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier3, t3_v0, "a-photo", 0, 210)?;

        let stats_mid = repo::ai::count_ai_queue_stats(conn, AiTier::Tier2, Some(AiTier::Tier3))?;
        assert_eq!(stats_mid.indexed_assets, 2);
        assert_eq!(stats_mid.staging_remaining, 1);

        let t2_video_vecs = repo::ai::list_vector_ids_for_asset(conn, AiTier::Tier2, "a-video")?;
        assert_eq!(t2_video_vecs, vec![1, 2]);
        let resolved_t2 = repo::ai::resolve_vision_vector_ids(conn, AiTier::Tier2, &[0, 2])?;
        assert_eq!(resolved_t2.get(&0), Some(&("a-photo".to_string(), 0)));
        assert_eq!(resolved_t2.get(&2), Some(&("a-video".to_string(), 12_000)));

        // Finish Tier 3 and cut over atomically.
        let t3_v1 = repo::ai::allocate_next_vision_vector_id(conn, AiTier::Tier3)?;
        assert_eq!(t3_v1, 1);
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier3, t3_v1, "a-video", 5_000, 220)?;

        let stats_done = repo::ai::count_ai_queue_stats(conn, AiTier::Tier2, Some(AiTier::Tier3))?;
        assert_eq!(stats_done.staging_remaining, 0);

        let cutover = repo::ai::commit_vision_tier_cutover(conn, AiTier::Tier3, 300)?;
        assert_eq!(cutover.active_vision_tier, AiTier::Tier3);
        assert_eq!(cutover.staging_vision_tier, None);

        // Clean up old Tier 2 rows without affecting Tier 3.
        let removed_t2 = repo::ai::delete_vision_embeddings_for_tier(conn, AiTier::Tier2)?;
        assert_eq!(removed_t2, 3);
        assert_eq!(
            repo::ai::list_vector_ids_for_asset(conn, AiTier::Tier3, "a-video")?,
            vec![1]
        );

        // Reset stage to 1 marks assets needing Stage 2.
        let reset = repo::ai::reset_all_assets_ai_stage_to_1(conn, 400)?;
        assert_eq!(reset, 2);
        assert_eq!(repo::ai::list_assets_needing_stage2(conn, 10)?.len(), 2);
        Ok(())
    })
    .await
    .expect("dual-tier vision test");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ocr_fts_and_search_candidates_enforce_strict_rbac_and_filters() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(move |conn| {
        seed_folder(conn, "fA", "Family", "/tmp/fA");
        seed_folder(conn, "fB", "Private", "/tmp/fB");

        seed_asset_with_file(conn, "a1", "fA", AssetKind::Photo, 1000, true, "a1.jpg");
        seed_asset_with_file(conn, "a2", "fA", AssetKind::Video, 2000, false, "a2.mp4");
        seed_asset_with_file(conn, "b1", "fB", AssetKind::Photo, 3000, true, "b1.jpg");

        conn.execute(
            "INSERT INTO deterministic_photo_meta (asset_id, camera_model, geo_city, geo_country)
             VALUES
             ('a1', 'Canon R5', 'Kyoto', 'Japan'),
             ('b1', 'Sony A7RV', 'Tokyo', 'Japan')",
            [],
        )?;

        conn.execute(
            "INSERT INTO people (id, name, is_hidden, is_pinned, created_at_ms, updated_at_ms)
             VALUES ('p1', 'Alice', 0, 0, 1000, 1000)",
            [],
        )?;
        repo::ai::insert_face_with_embedding(
            conn,
            "face-1",
            "a1",
            Some("p1"),
            (0.1, 0.1, 0.2, 0.2),
            0.95,
            0,
            "manual",
            1000,
        )?;

        repo::ai::insert_ai_tags_ignoring_rejected(
            conn,
            "a1",
            &[("receipt".to_string(), 0.91), ("temple".to_string(), 0.88)],
            "siglip2-large",
            1000,
        )?;

        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, 0, "a1", 0, 1000)?;
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, 1, "a2", 0, 1000)?;
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, 2, "a2", 5000, 1000)?;
        repo::ai::upsert_vision_embedding(conn, AiTier::Tier2, 3, "b1", 0, 1000)?;

        repo::ai::upsert_ocr_document(
            conn,
            "a1",
            "Kyoto Coffee Shop Receipt TOTAL $14.50 AT&T",
            Some("en"),
            "pp-ocrv6",
            1000,
        )?;
        repo::ai::upsert_ocr_document(
            conn,
            "b1",
            "Private Bank Receipt TOTAL $999.00",
            Some("en"),
            "pp-ocrv6",
            1000,
        )?;
        Ok(())
    })
    .await
    .expect("seed search test data");

    // Admin sees both Folder A and Folder B OCR hits.
    let admin_ocr = db
        .read(|conn| {
            repo::search::search_ocr_fts(conn, &RbacScope::AdminAll, "Receipt TOTAL", None, 20)
        })
        .await
        .expect("admin ocr search");
    assert_eq!(admin_ocr.len(), 2);
    let admin_ids: HashSet<String> = admin_ocr.iter().map(|(id, _, _)| id.clone()).collect();
    assert!(admin_ids.contains("a1"));
    assert!(admin_ids.contains("b1"));
    assert!(admin_ocr
        .iter()
        .all(|(_, score, snip)| *score > 0.0 && !snip.is_empty()));

    // Viewer restricted to [fA] sees ONLY a1, never b1.
    let viewer_scope = RbacScope::RestrictedFolders(vec!["fA".to_string()]);
    let viewer_ocr = db
        .read({
            let scope = viewer_scope.clone();
            move |conn| repo::search::search_ocr_fts(conn, &scope, "Receipt", None, 20)
        })
        .await
        .expect("viewer ocr search");
    assert_eq!(viewer_ocr.len(), 1);
    assert_eq!(viewer_ocr[0].0, "a1");

    // Hostile/malformed FTS5 input never triggers syntax error.
    for tricky in [
        "",
        "   ",
        "--- +++ ***",
        "AT&T",
        "\"unclosed quote OR NOT AND NEAR (foo:bar) ^",
    ] {
        let res = db
            .read({
                let q = tricky.to_string();
                move |conn| repo::search::search_ocr_fts(conn, &RbacScope::AdminAll, &q, None, 10)
            })
            .await
            .expect("fts5 query must never syntax error");
        if tricky == "AT&T" {
            assert_eq!(res.len(), 1);
            assert_eq!(res[0].0, "a1");
        }
    }

    // Filtered candidates & SQL-only mode enforce RBAC + all deterministic filters.
    let (admin_cands, viewer_cands, filtered_items, loaded_map) = db
        .read({
            let scope = viewer_scope.clone();
            move |conn| {
                let req_all = SearchRequest::default();
                let ac = repo::search::build_filtered_candidates(
                    conn,
                    &RbacScope::AdminAll,
                    &req_all,
                    AiTier::Tier2,
                )?;
                let vc =
                    repo::search::build_filtered_candidates(conn, &scope, &req_all, AiTier::Tier2)?;

                let req_filtered = SearchRequest {
                    mode: SearchMode::Filters,
                    person_ids: Some(vec!["p1".to_string()]),
                    tags: Some(vec!["TEMPLE".to_string()]),
                    camera_model: Some("canon".to_string()),
                    geo_query: Some("kyoto".to_string()),
                    kind: Some(AssetKind::Photo),
                    favorite: Some(true),
                    date_from_ms: Some(900),
                    date_to_ms: Some(1100),
                    ..Default::default()
                };
                let (items, total) =
                    repo::search::list_sql_filtered_assets(conn, &scope, &req_filtered)?;
                assert_eq!(total, 1);

                // Viewer attempting to filter directly to restricted folder fB gets empty.
                let req_forbidden = SearchRequest {
                    folder_id: Some("fB".to_string()),
                    ..Default::default()
                };
                let forbidden_cands = repo::search::build_filtered_candidates(
                    conn,
                    &scope,
                    &req_forbidden,
                    AiTier::Tier2,
                )?;
                assert!(forbidden_cands.asset_ids.is_empty());
                assert!(forbidden_cands.vector_ids.is_empty());

                let loaded = repo::search::load_asset_summaries_by_ids(
                    conn,
                    &scope,
                    &["a1".to_string(), "b1".to_string()],
                )?;
                Ok((ac, vc, items, loaded))
            }
        })
        .await
        .expect("filtered candidates and sql mode");

    assert_eq!(admin_cands.asset_ids.len(), 3);
    assert_eq!(admin_cands.vector_ids.len(), 4);
    assert_eq!(viewer_cands.asset_ids.len(), 2);
    assert!(!viewer_cands.asset_ids.contains("b1"));
    assert_eq!(viewer_cands.vector_ids.len(), 3);
    assert!(!viewer_cands.vector_ids.contains(3));

    assert_eq!(filtered_items.len(), 1);
    assert_eq!(filtered_items[0].id, "a1");
    assert_eq!(loaded_map.len(), 1);
    assert!(loaded_map.contains_key("a1"));
    assert!(!loaded_map.contains_key("b1"));

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn people_merge_reassignment_clustering_and_cover_invariant() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(move |conn| {
        seed_folder(conn, "fA", "Family", "/tmp/fA");
        seed_folder(conn, "fB", "Private", "/tmp/fB");

        seed_asset_with_file(conn, "a1", "fA", AssetKind::Photo, 1000, false, "a1.jpg");
        seed_asset_with_file(conn, "a2", "fA", AssetKind::Photo, 2000, false, "a2.jpg");
        seed_asset_with_file(conn, "b1", "fB", AssetKind::Photo, 3000, false, "b1.jpg");

        conn.execute(
            "INSERT INTO people (id, name, is_hidden, is_pinned, cover_face_id, created_at_ms, updated_at_ms)
             VALUES
             ('p-alice', 'Alice', 0, 0, 'f-a1', 1000, 1000),
             ('p-dup', NULL, 0, 0, 'f-b1', 1001, 1001),
             ('p-hidden', 'Hidden Person', 1, 0, 'f-a2', 1002, 1002)",
            [],
        )?;

        let v0 = repo::ai::allocate_next_face_vector_id(conn)?;
        let v1 = v0 + 1;
        let v2 = v0 + 2;
        let v3 = v0 + 3;
        repo::ai::insert_face_with_embedding(
            conn,
            "f-a1",
            "a1",
            Some("p-alice"),
            (0.1, 0.1, 0.2, 0.2),
            0.90,
            v0,
            "auto",
            1000,
        )?;
        repo::ai::insert_face_with_embedding(
            conn,
            "f-a2",
            "a2",
            Some("p-hidden"),
            (0.2, 0.2, 0.2, 0.2),
            0.85,
            v1,
            "auto",
            1001,
        )?;
        repo::ai::insert_face_with_embedding(
            conn,
            "f-b1",
            "b1",
            Some("p-dup"),
            (0.3, 0.3, 0.2, 0.2),
            0.99,
            v2,
            "auto",
            1002,
        )?;
        repo::ai::insert_face_with_embedding(
            conn,
            "f-unassigned",
            "a1",
            None,
            (0.5, 0.1, 0.15, 0.15),
            0.80,
            v3,
            "unassigned",
            1003,
        )?;

        repo::ai::upsert_person_centroid(conn, "p-dup", 1, &[1, 2, 3, 4], 1002)?;
        assert_eq!(repo::ai::list_unassigned_faces_with_vectors(conn)?.len(), 1);
        assert_eq!(repo::ai::list_person_centroids(conn)?.len(), 1);

        // Merge p-dup into p-alice.
        let (merged_person, moved_count) =
            repo::people::merge_people(conn, &["p-dup".to_string()], "p-alice", 2000)?
                .expect("merge target exists");
        assert_eq!(moved_count, 1);
        assert_eq!(merged_person.face_count, 2);
        assert!(repo::ai::list_person_centroids(conn)?.is_empty());

        // Pin p-alice's cover face to f-b1 (in fB!).
        let updated = repo::people::update_person(
            conn,
            "p-alice",
            &UpdatePersonRequest {
                is_pinned: Some(true),
                cover_face_id: Some("f-b1".to_string()),
                ..Default::default()
            },
            2100,
        )?
        .expect("updated p-alice");
        assert!(updated.is_pinned);
        assert_eq!(updated.cover_face_id.as_deref(), Some("f-b1"));
        Ok(())
    })
    .await
    .expect("seed and merge people");

    // Viewer [fA] sees p-alice with cover fallback f-a1 (NOT f-b1 from fB!), and NOT p-hidden.
    let viewer_scope = RbacScope::RestrictedFolders(vec!["fA".to_string()]);
    let (viewer_list, viewer_hidden_list, viewer_alice, viewer_assets, thumb_ok, thumb_denied) = db
        .read({
            let scope = viewer_scope.clone();
            move |conn| {
                let vis = repo::people::list_people(conn, &scope)?;
                let with_hidden = repo::people::list_people_filtered(conn, &scope, true)?;
                let alice = repo::people::get_person_by_id(conn, &scope, "p-alice")?;
                let (assets, next, total) =
                    repo::people::list_person_assets(conn, &scope, "p-alice", None, 10)?;
                assert!(next.is_none());
                assert_eq!(total, 1);
                let ok_thumb = repo::ai::get_face_with_asset_for_thumb(conn, &scope, "f-a1")?;
                let denied_thumb = repo::ai::get_face_with_asset_for_thumb(conn, &scope, "f-b1")?;
                Ok((vis, with_hidden, alice, assets, ok_thumb, denied_thumb))
            }
        })
        .await
        .expect("viewer people and face thumb RBAC");

    assert_eq!(viewer_list.len(), 1);
    assert_eq!(viewer_hidden_list.len(), 2);
    let alice = viewer_alice.expect("alice visible to viewer");
    assert_eq!(alice.face_count, 1);
    assert_eq!(alice.cover_face_id.as_deref(), Some("f-a1"));
    assert_eq!(viewer_assets.len(), 1);
    assert_eq!(viewer_assets[0].id, "a1");
    assert!(thumb_ok.is_some());
    assert!(thumb_denied.is_none());

    // Reassign f-a1 to a newly created person ("Carol"), and f-b1 to unassigned;
    // verify strict cover face invariant & assigned_by updates!
    db.write(move |conn| {
        let r1 = repo::people::reassign_face(conn, "f-a1", None, Some("Carol"), 3000)?
            .expect("f-a1 exists");
        let carol_id = r1.person_id.clone().expect("created Carol");

        let r2 = repo::people::reassign_face(conn, "f-b1", None, None, 3100)?.expect("f-b1 exists");
        assert!(r2.person_id.is_none());

        // Now p-alice has 0 active faces -> get_person_by_id returns None even for Admin!
        assert!(repo::people::get_person_by_id(conn, &RbacScope::AdminAll, "p-alice")?.is_none());

        // Carol has f-a1 with assigned_by = 'manual'.
        let faces = repo::people::list_asset_faces(conn, &viewer_scope, "a1")?;
        let f_a1 = faces.iter().find(|f| f.id == "f-a1").expect("f-a1 on a1");
        assert_eq!(f_a1.person_id.as_deref(), Some(carol_id.as_str()));
        assert_eq!(f_a1.person_name.as_deref(), Some("Carol"));
        assert_eq!(f_a1.assigned_by, "manual");

        // Create a cluster for f-unassigned + f-b1.
        let cluster_id = repo::people::create_person_for_cluster(
            conn,
            &["f-unassigned".to_string(), "f-b1".to_string()],
            &vec![0u8; 512 * 4],
            3200,
        )?;
        let vids = repo::people::list_face_vector_ids_for_person(conn, &cluster_id)?;
        assert_eq!(vids.len(), 2);

        // Strict cover invariant for viewer [fA]: cluster's global cover is f-b1 (det 0.99 in fB),
        // so viewer MUST fall back to f-unassigned (in fA)!
        let cluster_viewer = repo::people::get_person_by_id(conn, &viewer_scope, &cluster_id)?
            .expect("cluster visible in fA");
        assert_eq!(cluster_viewer.face_count, 1);
        assert_eq!(
            cluster_viewer.cover_face_id.as_deref(),
            Some("f-unassigned")
        );
        Ok(())
    })
    .await
    .expect("reassignment and clustering");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn insert_ai_tags_ignoring_rejected_never_overwrites_rejected_tags() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(move |conn| {
        seed_folder(conn, "f1", "Photos", "/tmp/f1");
        seed_asset_with_file(conn, "a1", "f1", AssetKind::Photo, 1000, false, "a1.jpg");

        // Initial AI tags
        let inserted = repo::ai::insert_ai_tags_ignoring_rejected(
            conn,
            "a1",
            &[("beach".to_string(), 0.92), ("ocean".to_string(), 0.85)],
            "siglip2-large",
            1000,
        )?;
        assert_eq!(inserted, 2);

        // User rejects "beach"
        let after_reject =
            repo::tags::remove_or_reject_tag(conn, &RbacScope::AdminAll, "a1", "beach")?
                .expect("asset visible");
        assert_eq!(after_reject.len(), 1);
        assert_eq!(after_reject[0].tag, "ocean");

        // Re-running AI zero-shot tagging with "beach" (including different case) + new tag "sunset"
        // must insert "sunset" and NEVER resurrect "beach".
        let inserted2 = repo::ai::insert_ai_tags_ignoring_rejected(
            conn,
            "a1",
            &[("Beach".to_string(), 0.99), ("sunset".to_string(), 0.77)],
            "siglip2-so400m",
            2000,
        )?;
        assert_eq!(inserted2, 1);

        let active = repo::tags::list_active_tags(conn, "a1")?;
        let names: Vec<&str> = active.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(names, vec!["ocean", "sunset"]);
        assert_eq!(active[1].source, TagSource::Ai);
        assert_eq!(active[1].state, TagState::Active);

        // Verify rejected row in media_tags remained untouched.
        let (state, conf): (String, f64) = conn.query_row(
            "SELECT state, confidence FROM media_tags WHERE asset_id = 'a1' AND tag = 'beach'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(state, "rejected");
        assert!((conf - 0.92).abs() < 1e-6);
        Ok(())
    })
    .await
    .expect("ai tags rejection respect");

    db.shutdown().await.expect("shutdown");
}
