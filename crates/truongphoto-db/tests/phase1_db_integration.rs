use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use truongphoto_core::models::{
    AlbumPermission, AlbumSharePermission, AlbumSortOrder, AssetKind, CacheEntryKind, FileRole,
    Histogram256, LruCacheEntry, PhotoMetaView, PlaybackMode, TagSource, TagState,
    UpdateOverridesRequest, VideoMetaView, HISTOGRAM_BINS,
};
use truongphoto_core::rbac::{RbacScope, Role};
use truongphoto_db::repo::assets::AssetListQuery;
use truongphoto_db::repo::folders::CreateFolder;
use truongphoto_db::repo::ingest::IngestedFileData;
use truongphoto_db::repo::users::CreateUser;
use truongphoto_db::{repo, DbHandle};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tp-db-p1-test-{}", uuid::Uuid::new_v4()));
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

fn seed_folder(conn: &Connection, id: &str, name: &str, root: &str, allow_disk_delete: bool) {
    repo::folders::create_folder(
        conn,
        &CreateFolder {
            id: id.to_string(),
            name: name.to_string(),
            root_path: root.to_string(),
            volume_uuid: Some(format!("vol-{id}")),
            volume_label: Some("TestVol".to_string()),
            is_managed_uploads: false,
            allow_disk_delete,
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

fn make_still_ingest(
    folder_id: &str,
    rel_path: &str,
    ext: &str,
    capture_at_ms: i64,
    cid: Option<&str>,
    is_raw: bool,
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
    IngestedFileData {
        asset_id_hint: None,
        folder_id: folder_id.to_string(),
        rel_path: rel_path.to_string(),
        mime_type: if is_raw {
            "image/x-canon-cr3".to_string()
        } else {
            "image/jpeg".to_string()
        },
        extension: ext.to_string(),
        file_size: 4096,
        mtime_ms: capture_at_ms,
        partial_blake3: format!("blake3-{rel_path}"),
        capture_at_ms,
        capture_tz_offset_m: None,
        width: Some(4032),
        height: Some(3024),
        duration_ms: None,
        thumb_rel_path: Some(format!("th/{lower_stem}.webp")),
        thumb_ready: !is_raw,
        photo_meta: Some(PhotoMetaView {
            camera_make: Some("Apple".to_string()),
            camera_model: Some("iPhone 16 Pro".to_string()),
            taken_at_ms: Some(capture_at_ms),
            ..Default::default()
        }),
        raw_exif_json: None,
        video_meta: None,
        raw_probe_json: None,
        histogram: None,
        content_identifier: cid.map(ToString::to_string),
        embedded_video_offset: None,
        embedded_video_length: None,
        is_raw,
        is_video: false,
        is_livp: false,
        parent_dir,
        lower_stem,
    }
}

fn make_video_ingest(
    folder_id: &str,
    rel_path: &str,
    ext: &str,
    capture_at_ms: i64,
    duration_ms: i64,
    cid: Option<&str>,
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
    IngestedFileData {
        asset_id_hint: None,
        folder_id: folder_id.to_string(),
        rel_path: rel_path.to_string(),
        mime_type: "video/quicktime".to_string(),
        extension: ext.to_string(),
        file_size: 16_384,
        mtime_ms: capture_at_ms,
        partial_blake3: format!("blake3-{rel_path}"),
        capture_at_ms,
        capture_tz_offset_m: None,
        width: Some(1920),
        height: Some(1080),
        duration_ms: Some(duration_ms),
        thumb_rel_path: Some(format!("th/{lower_stem}-vid.webp")),
        thumb_ready: true,
        photo_meta: None,
        raw_exif_json: None,
        video_meta: Some(VideoMetaView {
            duration_ms,
            video_codec: Some("h264".to_string()),
            bit_depth: Some(8),
            taken_at_ms: Some(capture_at_ms),
            ..Default::default()
        }),
        raw_probe_json: None,
        histogram: None,
        content_identifier: cid.map(ToString::to_string),
        embedded_video_offset: None,
        embedded_video_length: None,
        is_raw: false,
        is_video: true,
        is_livp: false,
        parent_dir,
        lower_stem,
    }
}

#[tokio::test]
async fn migration_0002_histograms_and_lru_cache_eviction() {
    let tmp = TempDir::new();
    let db_path = tmp.path().join("library.db");

    let db1 = DbHandle::open(&db_path).expect("open db");
    let migrations: Vec<(i64, String)> = db1
        .read(|conn| {
            let mut stmt =
                conn.prepare("SELECT version, name FROM schema_migrations ORDER BY version ASC")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .expect("read migrations");
    assert_eq!(
        migrations,
        vec![
            (1, "0001_initial".to_string()),
            (2, "0002_phase1_media".to_string()),
            (3, "0003_phase2_ai".to_string()),
            (4, "0004_phase3_chat_map".to_string()),
            (5, "0005_r5_perf_indexes".to_string()),
        ]
    );
    db1.shutdown().await.expect("shutdown db1");

    // Reopen idempotently and test histograms + LRU cache CRUD/eviction.
    let db = DbHandle::open(&db_path).expect("reopen db");
    db.write(|conn| {
        seed_folder(conn, "f1", "Photos", "/tmp/photos", false);
        let asset_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest("f1", "a.jpg", "jpg", 1_700_000_000_000, None, false),
        )?;

        let hist = Histogram256 {
            r: (0..HISTOGRAM_BINS).map(|i| (i as u16) * 200).collect(),
            g: vec![100; HISTOGRAM_BINS],
            b: vec![200; HISTOGRAM_BINS],
            luma: vec![150; HISTOGRAM_BINS],
        };
        repo::assets::upsert_asset_histogram(conn, &asset_id, &hist)?;

        let detail = repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &asset_id)?
            .expect("asset detail");
        assert_eq!(detail.histogram.as_ref(), Some(&hist));

        repo::cache::upsert_cache_entry(
            conn,
            &LruCacheEntry {
                cache_key: "k-old".to_string(),
                asset_id: Some(asset_id.clone()),
                kind: CacheEntryKind::Preview2048,
                rel_path: "previews/old.webp".to_string(),
                size_bytes: 1_000,
                last_accessed_ms: 100,
                created_at_ms: 100,
            },
        )?;
        repo::cache::upsert_cache_entry(
            conn,
            &LruCacheEntry {
                cache_key: "k-mid".to_string(),
                asset_id: Some(asset_id.clone()),
                kind: CacheEntryKind::RawFull,
                rel_path: "raw/mid.jpg".to_string(),
                size_bytes: 2_000,
                last_accessed_ms: 200,
                created_at_ms: 200,
            },
        )?;
        repo::cache::upsert_cache_entry(
            conn,
            &LruCacheEntry {
                cache_key: "k-new".to_string(),
                asset_id: Some(asset_id.clone()),
                kind: CacheEntryKind::HlsProxy,
                rel_path: "hls/new".to_string(),
                size_bytes: 3_000,
                last_accessed_ms: 300,
                created_at_ms: 300,
            },
        )?;

        assert_eq!(repo::cache::total_cache_bytes(conn)?, 6_000);

        // Touch k-old so k-mid becomes oldest!
        repo::cache::touch_cache_entry(conn, "k-old", 400)?;
        assert_eq!(
            repo::cache::get_cache_entry(conn, "k-old")?
                .expect("k-old")
                .last_accessed_ms,
            400
        );

        let victims = repo::cache::pop_oldest_entries_to_free(conn, 2_500, None)?;
        let victim_keys: Vec<&str> = victims.iter().map(|v| v.cache_key.as_str()).collect();
        assert_eq!(victim_keys, vec!["k-mid", "k-new"]);
        assert_eq!(repo::cache::total_cache_bytes(conn)?, 1_000);

        let removed = repo::cache::remove_cache_entries_for_asset(conn, &asset_id)?;
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].cache_key, "k-old");
        assert_eq!(repo::cache::total_cache_bytes(conn)?, 0);

        Ok(())
    })
    .await
    .expect("histogram and lru cache checks");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn idempotent_stacking_both_orders_and_duration_guard() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        seed_folder(conn, "f1", "CameraRoll", "/tmp/roll", false);
        seed_user(conn, "u1", "alice", "Alice", Role::Contributor);

        // 1A. Live Photo: .MOV arrives FIRST, then .JPG arrives SECOND (with different edited stem + matching ContentIdentifier).
        let mov_first_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_video_ingest(
                "f1",
                "trip/IMG_1024.MOV",
                "mov",
                1_700_000_000_000,
                2_400,
                Some("CID-APPLE-1"),
            ),
        )?;
        // Attach a tag and favorite to the temp video asset to verify merge preserves them.
        repo::tags::add_user_tag(conn, &RbacScope::AdminAll, &mov_first_id, "u1", "live-tag")?;
        repo::assets::set_asset_favorite(conn, &RbacScope::AdminAll, &mov_first_id, true)?;

        let merged_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "trip/IMG_E1024.JPG",
                "jpg",
                1_700_000_000_000,
                Some("CID-APPLE-1"),
                false,
            ),
        )?;
        assert_ne!(merged_id, mov_first_id);
        let detail_a = repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &merged_id)?
            .expect("merged live_photo detail");
        assert_eq!(detail_a.summary.kind, AssetKind::LivePhoto);
        assert!(detail_a.summary.is_favorite);
        assert_eq!(detail_a.files.len(), 2);
        assert_eq!(detail_a.files[0].file_role, FileRole::Primary);
        assert_eq!(detail_a.files[1].file_role, FileRole::LiveVideo);
        assert_eq!(detail_a.tags.len(), 1);
        assert_eq!(detail_a.tags[0].tag, "live-tag");
        assert!(detail_a.playback.live_video_url.is_some());

        // Temp video asset should be gone
        assert!(
            repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &mov_first_id)?.is_none()
        );

        // 1B. Live Photo: .JPG arrives FIRST, then .MOV arrives SECOND.
        let jpg_first_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "trip/IMG_2000.JPG",
                "jpg",
                1_700_000_010_000,
                Some("CID-APPLE-2"),
                false,
            ),
        )?;
        let mov_second_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_video_ingest(
                "f1",
                "trip/IMG_2000.MOV",
                "mov",
                1_700_000_010_000,
                1_900,
                Some("CID-APPLE-2"),
            ),
        )?;
        assert_eq!(jpg_first_id, mov_second_id);
        let detail_b = repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &jpg_first_id)?
            .expect("jpg-first live_photo");
        assert_eq!(detail_b.summary.kind, AssetKind::LivePhoto);
        assert_eq!(detail_b.files.len(), 2);

        // 1C. 30-second video with same stem and NO ContentIdentifier must NOT collapse!
        let still_30s_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "trip/DSC_0099.JPG",
                "jpg",
                1_700_000_020_000,
                None,
                false,
            ),
        )?;
        let vid_30s_id = repo::ingest::reconcile_ingested_file(
            conn,
            &make_video_ingest(
                "f1",
                "trip/DSC_0099.MP4",
                "mp4",
                1_700_000_020_000,
                30_000,
                None,
            ),
        )?;
        assert_ne!(still_30s_id, vid_30s_id);
        assert_eq!(
            repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &still_30s_id)?
                .unwrap()
                .summary
                .kind,
            AssetKind::Photo
        );
        let vid_detail =
            repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &vid_30s_id)?.unwrap();
        assert_eq!(vid_detail.summary.kind, AssetKind::Video);
        assert_eq!(vid_detail.playback.mode, PlaybackMode::Direct);

        // 2A. RAW + JPG pair: .CR3 first, then .JPG
        let cr3_first = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "raw/IMG_500.CR3",
                "cr3",
                1_700_000_030_000,
                None,
                true,
            ),
        )?;
        let jpg_second = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "raw/IMG_500.JPG",
                "jpg",
                1_700_000_030_000,
                None,
                false,
            ),
        )?;
        assert_eq!(cr3_first, jpg_second);
        let raw_stack_a =
            repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &cr3_first)?.unwrap();
        assert_eq!(raw_stack_a.summary.kind, AssetKind::RawJpgStack);
        assert!(raw_stack_a.summary.thumb_ready);
        assert!(raw_stack_a.playback.has_raw);
        assert_eq!(raw_stack_a.files[0].file_role, FileRole::Raw);
        assert_eq!(raw_stack_a.files[1].file_role, FileRole::JpegCompanion);

        // 2B. RAW + JPG pair: .JPG first, then .CR3
        let jpg_first = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "raw/IMG_501.JPG",
                "jpg",
                1_700_000_040_000,
                None,
                false,
            ),
        )?;
        let cr3_second = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "raw/IMG_501.CR3",
                "cr3",
                1_700_000_040_000,
                None,
                true,
            ),
        )?;
        assert_eq!(jpg_first, cr3_second);
        let raw_stack_b =
            repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &jpg_first)?.unwrap();
        assert_eq!(raw_stack_b.summary.kind, AssetKind::RawJpgStack);
        assert_eq!(raw_stack_b.files[0].file_role, FileRole::Raw);
        assert_eq!(raw_stack_b.files[1].file_role, FileRole::JpegCompanion);

        // 3. Re-ingesting the same path is idempotent
        let cr3_repeat = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest(
                "f1",
                "raw/IMG_501.CR3",
                "cr3",
                1_700_000_040_000,
                None,
                true,
            ),
        )?;
        assert_eq!(cr3_repeat, jpg_first);

        // 4. cleanup_missing_files downgrades split live_photo -> photo
        let live_mov_file_id = detail_b.files[1].id.clone();
        repo::ingest::mark_files_missing(conn, &[live_mov_file_id], 1_700_000_050_000)?;
        let cleaned = repo::ingest::cleanup_missing_files(conn, "f1", 1_700_000_051_000)?;
        assert_eq!(cleaned, 1);
        let downgraded =
            repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &jpg_first_id)?.unwrap();
        assert_eq!(downgraded.summary.kind, AssetKind::Photo);
        assert_eq!(downgraded.files.len(), 1);
        assert_eq!(downgraded.files[0].file_role, FileRole::Primary);

        Ok(())
    })
    .await
    .expect("idempotent stacking test");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn move_rename_relinking_preserves_asset_id_and_user_data() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        seed_folder(conn, "f1", "Folder1", "/tmp/f1", false);
        seed_folder(conn, "f2", "Folder2", "/tmp/f2", false);
        seed_user(conn, "u1", "ian", "Ian", Role::Admin);

        let mut data =
            make_still_ingest("f1", "old/photo.jpg", "jpg", 1_700_000_000_000, None, false);
        data.partial_blake3 = "deadbeef-partial-hash".to_string();
        data.file_size = 98_765;

        let asset_id = repo::ingest::reconcile_ingested_file(conn, &data)?;

        repo::assets::set_asset_favorite(conn, &RbacScope::AdminAll, &asset_id, true)?;
        repo::assets::upsert_user_overrides(
            conn,
            &RbacScope::AdminAll,
            &asset_id,
            "u1",
            &UpdateOverridesRequest {
                title: Some("Saved Title".to_string()),
                rating: Some(5),
                ..Default::default()
            },
        )?;
        repo::tags::add_user_tag(conn, &RbacScope::AdminAll, &asset_id, "u1", "vacation")?;
        let album = repo::albums::create_album(
            conn,
            "u1",
            "Memories",
            None,
            std::slice::from_ref(&asset_id),
            &RbacScope::AdminAll,
        )?;

        let index = repo::ingest::list_folder_file_index(conn, "f1")?;
        let entry = index.get("old/photo.jpg").expect("indexed file");
        repo::ingest::mark_files_missing(
            conn,
            std::slice::from_ref(&entry.file_id),
            1_700_000_010_000,
        )?;

        let missing =
            repo::ingest::find_missing_by_fingerprint(conn, "f1", "deadbeef-partial-hash", 98_765)?
                .expect("find missing by partial_blake3 + size");
        let relinked_asset_id = repo::ingest::relink_moved_file(
            conn,
            &missing.file_id,
            "f2",
            "moved/renamed.jpg",
            1_700_000_020_000,
            1_700_000_020_000,
        )?;

        assert_eq!(relinked_asset_id, asset_id);

        let detail = repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &asset_id)?
            .expect("detail after relink");
        assert_eq!(detail.summary.folder_id, "f2");
        assert!(detail.summary.is_favorite);
        assert_eq!(detail.summary.title.as_deref(), Some("Saved Title"));
        assert_eq!(detail.summary.rating, Some(5));
        assert_eq!(detail.tags.len(), 1);
        assert_eq!(detail.tags[0].tag, "vacation");
        assert_eq!(detail.album_ids, vec![album.id]);
        assert_eq!(detail.files[0].rel_path, "moved/renamed.jpg");
        assert!(!detail.files[0].is_missing);

        Ok(())
    })
    .await
    .expect("relink test");

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn albums_crud_share_rbac_and_tags_and_trash_purge() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        seed_folder(conn, "f1", "PublicFamily", "/tmp/f1", false);
        seed_folder(conn, "f2", "PrivateAdmin", "/tmp/f2", true);
        seed_user(conn, "u-admin", "admin", "Admin", Role::Admin);
        seed_user(conn, "u-contrib", "contrib", "Contributor", Role::Contributor);
        seed_user(conn, "u-viewer", "viewer", "Viewer", Role::Viewer);

        repo::users::replace_user_folder_acls(conn, "u-contrib", &["f1".to_string()])?;
        repo::users::replace_user_folder_acls(conn, "u-viewer", &["f1".to_string()])?;

        let a1 = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest("f1", "p1.jpg", "jpg", 1_700_000_100_000, None, false),
        )?;
        let a2 = repo::ingest::reconcile_ingested_file(
            conn,
            &make_still_ingest("f2", "p2.jpg", "jpg", 1_700_000_200_000, None, false),
        )?;

        // Before_ms scrubber jump test
        let (jumped, _, total_before) = repo::assets::list_assets(
            conn,
            &RbacScope::AdminAll,
            &AssetListQuery {
                before_ms: Some(1_700_000_150_000),
                ..Default::default()
            },
        )?;
        assert_eq!(total_before, 1);
        assert_eq!(jumped[0].id, a1);

        // Album CRUD + RBAC filtering
        let album = repo::albums::create_album(
            conn,
            "u-admin",
            "Holiday",
            Some("Family trip"),
            &[a2.clone(), a1.clone()],
            &RbacScope::AdminAll,
        )?;
        assert_eq!(album.asset_count, 2);
        assert_eq!(album.cover_asset_id.as_deref(), Some(a2.as_str()));

        let shares = repo::albums::replace_album_shares(
            conn,
            &album.id,
            "u-admin",
            &[
                ("u-viewer".to_string(), AlbumSharePermission::View),
                ("u-contrib".to_string(), AlbumSharePermission::Contribute),
            ],
        )?;
        assert_eq!(shares.len(), 2);
        assert_eq!(repo::albums::list_shareable_users(conn)?.len(), 3);

        let viewer_scope = RbacScope::RestrictedFolders(vec!["f1".to_string()]);
        let viewer_detail = repo::albums::get_album_detail(
            conn,
            &album.id,
            "u-viewer",
            Role::Viewer,
            &viewer_scope,
        )?
        .expect("viewer can open shared album");
        assert_eq!(viewer_detail.permission, AlbumPermission::View);
        assert_eq!(viewer_detail.assets.len(), 1);
        assert_eq!(viewer_detail.assets[0].id, a1);
        assert!(viewer_detail.album.cover_asset_id.is_none()); // a2 is in f2!

        // Contributor with contribute permission cannot add an asset from restricted f2
        let added_restricted = repo::albums::add_album_assets(
            conn,
            &album.id,
            "u-contrib",
            std::slice::from_ref(&a2),
            &viewer_scope,
        )?;
        assert_eq!(added_restricted, 0);

        let updated_album = repo::albums::update_album(
            conn,
            &album.id,
            Some("Holiday 2026"),
            None,
            Some(&a1),
            Some(AlbumSortOrder::Custom),
            &RbacScope::AdminAll,
        )?
        .expect("updated album");
        assert_eq!(updated_album.title, "Holiday 2026");
        assert_eq!(updated_album.sort_order, "custom");

        // Tags: rejecting AI tag sets state='rejected', removing user tag deletes row
        conn.execute(
            "INSERT INTO media_tags (asset_id, tag, source, state, confidence, created_at_ms, updated_at_ms)
             VALUES (?1, 'sunset', 'ai', 'active', 0.91, 1000, 1000)",
            params![a1],
        )?;
        let tags_after_add =
            repo::tags::add_user_tag(conn, &viewer_scope, &a1, "u-contrib", "beach")?
                .expect("asset visible");
        assert_eq!(tags_after_add.len(), 2);

        let tags_after_reject =
            repo::tags::remove_or_reject_tag(conn, &viewer_scope, &a1, "sunset")?.unwrap();
        assert_eq!(tags_after_reject.len(), 1);
        assert_eq!(tags_after_reject[0].tag, "beach");
        assert_eq!(tags_after_reject[0].source, TagSource::User);

        let ai_state: String = conn.query_row(
            "SELECT state FROM media_tags WHERE asset_id = ?1 AND tag = 'sunset'",
            params![a1],
            |r| r.get(0),
        )?;
        assert_eq!(ai_state, TagState::Rejected.as_str());

        let tags_after_user_del =
            repo::tags::remove_or_reject_tag(conn, &viewer_scope, &a1, "beach")?.unwrap();
        assert!(tags_after_user_del.is_empty());
        let user_row_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM media_tags WHERE asset_id = ?1 AND tag = 'beach'",
                params![a1],
                |r| r.get(0),
            )
            .optional()?;
        assert!(user_row_exists.is_none());

        // Non-destructive partial overrides
        repo::assets::upsert_user_overrides(
            conn,
            &viewer_scope,
            &a1,
            "u-contrib",
            &UpdateOverridesRequest {
                title: Some("Beach Sunset".to_string()),
                description: Some("Keep me".to_string()),
                ..Default::default()
            },
        )?;
        repo::assets::upsert_user_overrides(
            conn,
            &viewer_scope,
            &a1,
            "u-contrib",
            &UpdateOverridesRequest {
                rating: Some(4),
                ..Default::default()
            },
        )?;
        let a1_detail = repo::assets::get_asset_detail(conn, &viewer_scope, &a1)?.unwrap();
        let ov = a1_detail.overrides.expect("overrides");
        assert_eq!(ov.title.as_deref(), Some("Beach Sunset"));
        assert_eq!(ov.description.as_deref(), Some("Keep me"));
        assert_eq!(ov.rating, Some(4));

        // Trash, Restore, Expired Trash & Purge (hide_only vs hard delete)
        let trashed_a1 = repo::assets::soft_delete_assets(
            conn,
            &RbacScope::AdminAll,
            "u-admin",
            std::slice::from_ref(&a1),
            1_000_000,
        )?;
        let trashed_a2 = repo::assets::soft_delete_assets(
            conn,
            &RbacScope::AdminAll,
            "u-admin",
            std::slice::from_ref(&a2),
            1_000_500,
        )?;
        assert_eq!(trashed_a1 + trashed_a2, 2);
        assert_eq!(
            repo::assets::list_expired_trash(conn, 1_000_200)?,
            vec![a1.clone()]
        );
        assert_eq!(
            repo::assets::list_expired_trash(conn, 1_000_500)?,
            vec![a1.clone(), a2.clone()]
        );

        let candidates = repo::assets::get_assets_for_purge(conn, None)?;
        assert_eq!(candidates.len(), 2);
        for c in candidates {
            repo::assets::delete_purged_asset_records(conn, &c.asset_id, !c.allow_disk_delete)?;
        }

        // a1 in f1 (allow_disk_delete=false) is hidden (not in timeline, not in trash, asset_files preserved)
        assert!(repo::assets::get_asset_detail(conn, &RbacScope::AdminAll, &a1)?.is_none());
        assert_eq!(repo::assets::list_trash(conn, &RbacScope::AdminAll)?.1, 0);
        let a1_files: i64 = conn.query_row(
            "SELECT COUNT(*) FROM asset_files WHERE asset_id = ?1",
            params![a1],
            |r| r.get(0),
        )?;
        assert_eq!(a1_files, 1);

        // a2 in f2 (allow_disk_delete=true) is completely deleted
        let a2_exists: Option<i64> = conn
            .query_row("SELECT 1 FROM assets WHERE id = ?1", params![a2], |r| r.get(0))
            .optional()?;
        assert!(a2_exists.is_none());

        assert!(repo::albums::delete_album(conn, &album.id)?);

        Ok(())
    })
    .await
    .expect("albums, tags, overrides, trash/purge test");

    db.shutdown().await.expect("shutdown");
}
