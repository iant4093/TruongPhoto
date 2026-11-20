use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use truongphoto_core::models::{
    AiTier, AssetKind, ChatHitRef, ChatRole, ChatTier, PhotoMetaView, PlannerMode, RouterPlan,
    UpdateOverridesRequest,
};
use truongphoto_core::rbac::{RbacScope, Role};
use truongphoto_db::repo::folders::CreateFolder;
use truongphoto_db::repo::ingest::IngestedFileData;
use truongphoto_db::repo::users::CreateUser;
use truongphoto_db::{repo, DbHandle};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tp-db-p3-test-{}", uuid::Uuid::new_v4()));
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

fn seed_user(conn: &Connection, id: &str, username: &str, display_name: &str, role: Role) {
    repo::users::create_user(
        conn,
        &CreateUser {
            id: id.to_string(),
            username: username.to_string(),
            display_name: display_name.to_string(),
            role,
            pin_hash: "phc-test".to_string(),
            avatar_color: "#4a4a4a".to_string(),
        },
    )
    .expect("create user");
}

fn make_photo_ingest(
    folder_id: &str,
    rel_path: &str,
    capture_at_ms: i64,
    gps: Option<(f64, f64, Option<&str>, Option<&str>)>,
) -> IngestedFileData {
    let path = Path::new(rel_path);
    let parent_dir = path
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let lower_stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let (gps_lat, gps_lon, geo_city, geo_country) = match gps {
        Some((lat, lon, city, country)) => (
            Some(lat),
            Some(lon),
            city.map(ToString::to_string),
            country.map(ToString::to_string),
        ),
        None => (None, None, None, None),
    };

    IngestedFileData {
        asset_id_hint: None,
        folder_id: folder_id.to_string(),
        rel_path: rel_path.to_string(),
        mime_type: "image/jpeg".to_string(),
        extension: "jpg".to_string(),
        file_size: 2048,
        mtime_ms: capture_at_ms,
        partial_blake3: format!("b3-{folder_id}-{rel_path}"),
        capture_at_ms,
        capture_tz_offset_m: None,
        width: Some(4000),
        height: Some(3000),
        duration_ms: None,
        thumb_rel_path: Some(format!("th/{lower_stem}.webp")),
        thumb_ready: true,
        photo_meta: Some(PhotoMetaView {
            camera_make: Some("Fujifilm".to_string()),
            camera_model: Some("X100VI".to_string()),
            gps_lat,
            gps_lon,
            geo_city,
            geo_country,
            taken_at_ms: Some(capture_at_ms),
            ..Default::default()
        }),
        raw_exif_json: None,
        video_meta: None,
        raw_probe_json: None,
        histogram: None,
        content_identifier: None,
        embedded_video_offset: None,
        embedded_video_length: None,
        is_raw: false,
        is_video: false,
        is_livp: false,
        parent_dir,
        lower_stem,
    }
}

#[tokio::test]
async fn migration_0004_idempotency_backfill_and_chat_tier_state() {
    let tmp = TempDir::new();
    let db_path = tmp.path().join("library.db");

    // Simulate a Phase 2 database (migrations 0001..0003 applied) with existing geotagged assets.
    {
        let mut raw = Connection::open(&db_path).expect("open raw sqlite");
        raw.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                 version       INTEGER PRIMARY KEY,
                 name          TEXT NOT NULL,
                 applied_at_ms INTEGER NOT NULL
             );",
        )
        .expect("init schema_migrations");

        let m1 = include_str!("../migrations/0001_initial.sql");
        let m2 = include_str!("../migrations/0002_phase1_media.sql");
        let m3 = include_str!("../migrations/0003_phase2_ai.sql");
        for (ver, name, sql) in [
            (1i64, "0001_initial", m1),
            (2i64, "0002_phase1_media", m2),
            (3i64, "0003_phase2_ai", m3),
        ] {
            let tx = raw.transaction().expect("begin tx");
            tx.execute_batch(sql).expect("apply pre-p3 migration");
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, 1000)",
                params![ver, name],
            )
            .expect("record migration");
            tx.commit().expect("commit migration");
        }

        seed_folder(&raw, "f1", "Travel", "/tmp/travel");
        for (id, kind, cap, hidden, del) in [
            ("a-photo", "photo", 1000i64, 0i64, None),
            ("a-video", "video", 2000, 0, None),
            ("a-override", "photo", 3000, 0, None),
            ("a-deleted", "photo", 4000, 0, Some(5000i64)),
            ("a-hidden", "photo", 5000, 1, None),
            ("a-nogps", "photo", 6000, 0, None),
        ] {
            raw.execute(
                "INSERT INTO assets (
                    id, folder_id, kind, capture_at_ms, is_favorite, is_hidden,
                    deleted_at_ms, thumb_ready, ai_stage, created_at_ms, updated_at_ms
                 ) VALUES (?1, 'f1', ?2, ?3, 0, ?4, ?5, 1, 0, 1000, 1000)",
                params![id, kind, cap, hidden, del],
            )
            .expect("insert pre-migration asset");
        }

        raw.execute(
            "INSERT INTO deterministic_photo_meta (
                asset_id, gps_lat, gps_lon, geo_city, geo_country, taken_at_ms
             ) VALUES
             ('a-photo', 35.6762, 139.6503, 'Tokyo', 'Japan', 1500),
             ('a-override', 10.0, 20.0, 'OldCity', 'OldCountry', 3000),
             ('a-deleted', 48.8566, 2.3522, 'Paris', 'France', 4000),
             ('a-hidden', 51.5074, -0.1278, 'London', 'UK', 5000)",
            [],
        )
        .expect("insert photo meta");
        raw.execute(
            "INSERT INTO deterministic_video_meta (
                asset_id, duration_ms, gps_lat, gps_lon, geo_city, geo_country, taken_at_ms
             ) VALUES ('a-video', 5000, 35.0116, 135.7681, 'Kyoto', 'Japan', 2500)",
            [],
        )
        .expect("insert video meta");
        raw.execute(
            "INSERT INTO user_overrides (
                asset_id, gps_lat, gps_lon, geo_city, geo_country, updated_at_ms
             ) VALUES ('a-override', 37.7749, -122.4194, 'San Francisco', 'USA', 9000)",
            [],
        )
        .expect("insert override");
    }

    // Open with DbHandle -> applies 0004 migration & backfills asset_geo_index.
    let db1 = DbHandle::open(&db_path).expect("open db1 and run 0004");
    let (versions, state, geo_count, points) = db1
        .read(|conn| {
            let mut stmt =
                conn.prepare("SELECT version, name FROM schema_migrations ORDER BY version ASC")?;
            let versions = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let state = repo::ai::get_ai_index_state(conn)?;
            let geo_count = repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?;
            let points = repo::map::list_map_points(
                conn,
                &RbacScope::AdminAll,
                -90.0,
                90.0,
                -180.0,
                180.0,
                50,
            )?;
            Ok((versions, state, geo_count, points))
        })
        .await
        .expect("inspect migrated state");

    assert_eq!(
        versions,
        vec![
            (1, "0001_initial".to_string()),
            (2, "0002_phase1_media".to_string()),
            (3, "0003_phase2_ai".to_string()),
            (4, "0004_phase3_chat_map".to_string()),
        ]
    );
    assert_eq!(state.active_vision_tier, AiTier::Tier2);
    assert_eq!(state.active_chat_tier, ChatTier::Tier0);
    assert!(!state.vlm_rerank_enabled);

    // Only active geotagged assets (a-override, a-video, a-photo) should be backfilled.
    assert_eq!(geo_count, 3);
    let backfilled_ids: Vec<&str> = points.items.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(backfilled_ids, vec!["a-override", "a-video", "a-photo"]);
    assert_eq!(points.items[0].gps_lat, Some(37.7749));
    assert_eq!(points.items[0].geo_city.as_deref(), Some("San Francisco"));
    db1.shutdown().await.expect("shutdown db1");

    // Reopen idempotently and verify set_chat_tier mutations persist.
    let db2 = DbHandle::open(&db_path).expect("reopen db2 idempotently");
    db2.write(|conn| {
        let s1 = repo::ai::set_chat_tier(conn, ChatTier::Tier3, Some(true), 12_000)?;
        assert_eq!(s1.active_chat_tier, ChatTier::Tier3);
        assert!(s1.vlm_rerank_enabled);
        assert_eq!(s1.updated_at_ms, 12_000);

        // Passing None for vlm_rerank_enabled preserves the existing toggle.
        let s2 = repo::ai::set_chat_tier(conn, ChatTier::Tier2, None, 13_000)?;
        assert_eq!(s2.active_chat_tier, ChatTier::Tier2);
        assert!(s2.vlm_rerank_enabled);
        Ok(())
    })
    .await
    .expect("update chat tier");
    db2.shutdown().await.expect("shutdown db2");
}

#[tokio::test]
async fn chat_sessions_and_messages_enforce_strict_user_id_isolation() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        seed_user(conn, "u-alice", "alice", "Alice", Role::Admin);
        seed_user(conn, "u-bob", "bob", "Bob", Role::Viewer);

        let alice_s1 = repo::chat::create_chat_session(conn, "u-alice", "Tokyo Trip", 1000)?;
        let alice_s2 = repo::chat::create_chat_session(conn, "u-alice", "   ", 2000)?;
        assert_eq!(alice_s2.title, "New Chat");

        let bob_s1 = repo::chat::create_chat_session(conn, "u-bob", "Bob Private", 1500)?;

        // User B cannot see, fetch, update, or delete User A's sessions.
        let bob_sessions = repo::chat::list_chat_sessions(conn, "u-bob")?;
        assert_eq!(bob_sessions.len(), 1);
        assert_eq!(bob_sessions[0].id, bob_s1.id);

        assert!(repo::chat::get_chat_session(conn, "u-bob", &alice_s1.id)?.is_none());
        assert!(!repo::chat::delete_chat_session(
            conn,
            "u-bob",
            &alice_s1.id
        )?);
        assert!(!repo::chat::update_chat_session_filters(
            conn,
            "u-bob",
            &alice_s1.id,
            &RouterPlan::default(),
            Some("Hijacked"),
            3000,
        )?);

        // User A appends turns and updates carried-forward filters.
        let user_msg = repo::chat::append_chat_message(
            conn,
            &alice_s1.id,
            ChatRole::User,
            "beach videos in Tokyo",
            None,
            None,
            false,
            None,
            None,
            2100,
        )?;
        assert_eq!(user_msg.role, ChatRole::User);
        assert!(user_msg.hit_refs.is_empty());

        let plan = RouterPlan {
            semantic_query: Some("beach".to_string()),
            location_query: Some("Tokyo".to_string()),
            kind: Some(AssetKind::Video),
            assistant_reply: "Found 1 beach video in Tokyo.".to_string(),
            follow_up_suggestions: vec!["Favorites only".to_string()],
            ..Default::default()
        };
        let hit_refs = vec![ChatHitRef {
            asset_id: "asset-1".to_string(),
            score: 0.94,
            timestamp_sec: Some(4.5),
        }];
        let assistant_msg = repo::chat::append_chat_message(
            conn,
            &alice_s1.id,
            ChatRole::Assistant,
            "Found 1 beach video in Tokyo.",
            Some(&plan),
            Some(&hit_refs),
            true,
            Some(PlannerMode::ExactSimd),
            Some(37),
            2200,
        )?;
        assert!(assistant_msg.reranked);
        assert_eq!(assistant_msg.planner_mode, Some(PlannerMode::ExactSimd));
        assert_eq!(assistant_msg.elapsed_ms, Some(37));

        assert!(repo::chat::update_chat_session_filters(
            conn,
            "u-alice",
            &alice_s1.id,
            &plan,
            Some("Tokyo Beach Videos"),
            2500,
        )?);

        // Alice's session list is ordered by updated_at_ms DESC (s1=2500 > s2=2000).
        let alice_list = repo::chat::list_chat_sessions(conn, "u-alice")?;
        assert_eq!(alice_list.len(), 2);
        assert_eq!(alice_list[0].id, alice_s1.id);
        assert_eq!(alice_list[0].title, "Tokyo Beach Videos");

        let detail =
            repo::chat::get_chat_session(conn, "u-alice", &alice_s1.id)?.expect("alice owns s1");
        assert_eq!(detail.active_filters, plan);

        let msgs = repo::chat::list_chat_messages(conn, &alice_s1.id)?;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, user_msg.id);
        assert_eq!(msgs[1].id, assistant_msg.id);
        assert_eq!(msgs[1].plan.as_ref(), Some(&plan));
        assert_eq!(msgs[1].hit_refs, hit_refs);
        assert!(msgs[1].reranked);

        // Deleting Alice's session by Alice succeeds and cascades messages.
        assert!(repo::chat::delete_chat_session(
            conn,
            "u-alice",
            &alice_s1.id
        )?);
        assert!(repo::chat::list_chat_messages(conn, &alice_s1.id)?.is_empty());
        assert!(repo::chat::get_chat_session(conn, "u-alice", &alice_s1.id)?.is_none());
        Ok(())
    })
    .await
    .expect("chat isolation tests");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn asset_geo_index_syncs_on_ingest_overrides_soft_delete_restore_and_purge() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        seed_folder(conn, "f1", "Library", "/tmp/lib");
        seed_user(conn, "u1", "admin", "Admin", Role::Admin);

        // 1. Ingest non-geotagged asset -> should not appear in geo index.
        let a_no_gps = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest("f1", "nogps.jpg", 1000, None),
        )?;
        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            0
        );

        // 2. Ingest geotagged asset -> automatically indexed.
        let a_geo = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "f1",
                "kyoto.jpg",
                2000,
                Some((35.0116, 135.7681, Some("Kyoto"), Some("Japan"))),
            ),
        )?;
        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            1
        );

        // 3. User override adds GPS to a_no_gps -> appears on map immediately.
        repo::assets::upsert_user_overrides(
            conn,
            &RbacScope::AdminAll,
            &a_no_gps,
            "u1",
            &UpdateOverridesRequest {
                gps_lat: Some(35.6762),
                gps_lon: Some(139.6503),
                geo_city: Some("Tokyo".to_string()),
                geo_country: Some("Japan".to_string()),
                ..Default::default()
            },
        )?
        .expect("override applied");
        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            2
        );

        // 4. User override updates GPS on a_no_gps -> coordinates & city update in asset_geo_index.
        repo::assets::upsert_user_overrides(
            conn,
            &RbacScope::AdminAll,
            &a_no_gps,
            "u1",
            &UpdateOverridesRequest {
                gps_lat: Some(34.6937),
                gps_lon: Some(135.5023),
                geo_city: Some("Osaka".to_string()),
                ..Default::default()
            },
        )?
        .expect("second override applied");
        let pts =
            repo::map::list_map_points(conn, &RbacScope::AdminAll, 34.0, 35.0, 135.0, 136.0, 50)?;
        assert_eq!(pts.total_count, 1);
        assert_eq!(pts.items[0].id, a_no_gps);
        assert_eq!(pts.items[0].geo_city.as_deref(), Some("Osaka"));

        // 5. Soft delete removes asset from map.
        let deleted = repo::assets::soft_delete_assets(
            conn,
            &RbacScope::AdminAll,
            "u1",
            std::slice::from_ref(&a_geo),
            5000,
        )?;
        assert_eq!(deleted, 1);
        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            1
        );

        // 6. Restore brings asset back onto map.
        let restored = repo::assets::restore_assets(
            conn,
            &RbacScope::AdminAll,
            std::slice::from_ref(&a_geo),
            6000,
        )?;
        assert_eq!(restored, 1);
        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            2
        );

        // 7. Hide-only purge removes asset permanently from map.
        repo::assets::soft_delete_assets(
            conn,
            &RbacScope::AdminAll,
            "u1",
            std::slice::from_ref(&a_geo),
            7000,
        )?;
        repo::assets::delete_purged_asset_records(conn, &a_geo, true)?;
        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            1
        );

        Ok(())
    })
    .await
    .expect("geo index sync lifecycle");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn map_clustering_and_points_enforce_rbac_and_antimeridian_wraparound() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        seed_folder(conn, "fA", "Family", "/tmp/fA");
        seed_folder(conn, "fB", "Private", "/tmp/fB");

        // Folder A: 2 in Tokyo + Fiji (178.44) + Samoa (-171.76)
        let _a_tokyo_old = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fA",
                "tokyo1.jpg",
                1000,
                Some((35.6762, 139.6503, Some("Tokyo"), Some("Japan"))),
            ),
        )?;
        let a_tokyo_new = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fA",
                "tokyo2.jpg",
                2000,
                Some((35.6895, 139.6610, Some("Tokyo"), Some("Japan"))),
            ),
        )?;
        let a_fiji = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fA",
                "fiji.jpg",
                3000,
                Some((-18.1416, 178.4419, Some("Suva"), Some("Fiji"))),
            ),
        )?;
        let a_samoa = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fA",
                "samoa.jpg",
                4000,
                Some((-13.8333, -171.7667, Some("Apia"), Some("Samoa"))),
            ),
        )?;

        // Folder B: Tokyo (newest overall!), Greenwich (0.0), Fiji (178.45)
        let _b_tokyo = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fB",
                "b_tokyo.jpg",
                9000,
                Some((35.6800, 139.6600, Some("Tokyo"), Some("Japan"))),
            ),
        )?;
        let _b_zero = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fB",
                "b_zero.jpg",
                9100,
                Some((-10.0, 0.0, Some("Ocean"), None)),
            ),
        )?;
        let _b_fiji = repo::ingest::reconcile_ingested_file(
            conn,
            &make_photo_ingest(
                "fB",
                "b_fiji.jpg",
                9200,
                Some((-18.1500, 178.4500, Some("Suva"), Some("Fiji"))),
            ),
        )?;

        let viewer_scope = RbacScope::RestrictedFolders(vec!["fA".to_string()]);
        let empty_scope = RbacScope::RestrictedFolders(Vec::new());

        assert_eq!(
            repo::map::count_geotagged_assets(conn, &RbacScope::AdminAll)?,
            7
        );
        assert_eq!(repo::map::count_geotagged_assets(conn, &viewer_scope)?, 4);
        assert_eq!(repo::map::count_geotagged_assets(conn, &empty_scope)?, 0);

        // Tokyo cluster check: Viewer sees ONLY 2 Folder A points & cover_asset_id is a_tokyo_new (NOT b_tokyo!)
        let viewer_tokyo =
            repo::map::list_map_clusters(conn, &viewer_scope, 35.0, 36.0, 139.0, 140.0, 6)?;
        assert_eq!(viewer_tokyo.total_in_view, 2);
        assert_eq!(viewer_tokyo.clusters.len(), 1);
        assert_eq!(viewer_tokyo.clusters[0].count, 2);
        assert_eq!(viewer_tokyo.clusters[0].cover_asset_id, a_tokyo_new);
        assert_eq!(viewer_tokyo.clusters[0].label.as_deref(), Some("Tokyo"));

        let admin_tokyo =
            repo::map::list_map_clusters(conn, &RbacScope::AdminAll, 35.0, 36.0, 139.0, 140.0, 6)?;
        assert_eq!(admin_tokyo.total_in_view, 3);
        assert_eq!(admin_tokyo.clusters[0].count, 3);

        // At higher zoom (zoom=12), the two Folder A Tokyo points split into separate cells.
        let viewer_tokyo_zoomed =
            repo::map::list_map_clusters(conn, &viewer_scope, 35.0, 36.0, 139.0, 140.0, 12)?;
        assert_eq!(viewer_tokyo_zoomed.total_in_view, 2);
        assert_eq!(viewer_tokyo_zoomed.clusters.len(), 2);

        // Anti-meridian bbox: min_lng = 170.0, max_lng = -170.0
        // Matches Fiji (178.x) and Samoa (-171.76), excludes 0.0!
        let admin_antimeridian =
            repo::map::list_map_clusters(conn, &RbacScope::AdminAll, -30.0, 0.0, 170.0, -170.0, 4)?;
        assert_eq!(admin_antimeridian.total_in_view, 3);

        let viewer_antimeridian =
            repo::map::list_map_clusters(conn, &viewer_scope, -30.0, 0.0, 170.0, -170.0, 4)?;
        assert_eq!(viewer_antimeridian.total_in_view, 2);

        let viewer_antimeridian_pts =
            repo::map::list_map_points(conn, &viewer_scope, -30.0, 0.0, 170.0, -170.0, 50)?;
        assert_eq!(viewer_antimeridian_pts.total_count, 2);
        let pt_ids: Vec<&str> = viewer_antimeridian_pts
            .items
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        // Ordered newest-first: a_samoa (4000) then a_fiji (3000)
        assert_eq!(pt_ids, vec![a_samoa.as_str(), a_fiji.as_str()]);

        Ok(())
    })
    .await
    .expect("map clustering and anti-meridian rbac tests");

    db.shutdown().await.expect("shutdown");
}
