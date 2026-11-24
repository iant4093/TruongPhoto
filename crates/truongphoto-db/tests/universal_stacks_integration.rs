use truongphoto_core::models::{AssetKind, PhotoMetaView};
use truongphoto_core::rbac::{RbacScope, Role};
use truongphoto_db::repo::assets::{list_assets, list_timeline_buckets_filtered, AssetListQuery};
use truongphoto_db::repo::folders::{create_folder, CreateFolder};
use truongphoto_db::repo::ingest::{reconcile_ingested_file, IngestedFileData};
use truongphoto_db::repo::stacks::{
    create_manual_stack, list_folder_subfolders, reconcile_all_stacks, remove_from_stack,
    set_stack_cover,
};
use truongphoto_db::DbHandle;

fn setup_db() -> (tempfile::TempDir, DbHandle) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = DbHandle::open(dir.path().join("test.db")).expect("open test db");
    (dir, db)
}

fn add_folder(conn: &rusqlite::Connection, id: &str, name: &str, root: &str) {
    create_folder(
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

#[allow(clippy::too_many_arguments)]
fn ingest_photo(
    conn: &rusqlite::Connection,
    folder_id: &str,
    rel_path: &str,
    ext: &str,
    taken_at_ms: i64,
    has_subsec: bool,
    tz_offset_m: Option<i32>,
    camera_model: Option<&str>,
    focal_mm: Option<f64>,
    aperture: Option<f64>,
    shutter: Option<f64>,
    iso: Option<u32>,
    software: Option<&str>,
    exif_modified_at_ms: Option<i64>,
    width: u32,
    height: u32,
    mtime_ms: i64,
    dhash64: Option<i64>,
) -> String {
    let is_raw = matches!(ext, "cr3" | "cr2" | "arw" | "nef" | "dng" | "raf");
    let path = std::path::Path::new(rel_path);
    let parent_dir = path
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let lower_stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let data = IngestedFileData {
        asset_id_hint: None,
        folder_id: folder_id.to_string(),
        rel_path: rel_path.to_string(),
        mime_type: if is_raw {
            "image/x-canon-cr3".to_string()
        } else {
            "image/jpeg".to_string()
        },
        extension: ext.to_string(),
        file_size: if is_raw { 35_000_000 } else { 8_000_000 },
        mtime_ms,
        partial_blake3: format!("b3-{folder_id}-{rel_path}"),
        capture_at_ms: taken_at_ms,
        capture_tz_offset_m: tz_offset_m,
        width: Some(width),
        height: Some(height),
        duration_ms: None,
        thumb_rel_path: Some(format!("th/{folder_id}_{lower_stem}.webp")),
        thumb_ready: true,
        dhash64,
        photo_meta: Some(PhotoMetaView {
            camera_make: Some("Canon".to_string()),
            camera_model: camera_model.map(ToOwned::to_owned),
            lens_model: Some("RF-S 10-18mm".to_string()),
            focal_length_mm: focal_mm,
            aperture_f: aperture,
            shutter_speed_sec: shutter,
            iso,
            taken_at_ms: Some(taken_at_ms),
            software: software.map(ToOwned::to_owned),
            exif_modified_at_ms,
            taken_at_has_subsec: has_subsec,
            ..Default::default()
        }),
        raw_exif_json: None,
        video_meta: None,
        raw_probe_json: None,
        histogram: None,
        content_identifier: None,
        embedded_video_offset: None,
        embedded_video_length: None,
        is_raw,
        is_video: false,
        is_livp: false,
        parent_dir,
        lower_stem,
    };
    reconcile_ingested_file(conn, &data).expect("reconcile_ingested_file")
}

#[tokio::test]
async fn universal_cross_folder_exif_stacking_ignores_filenames_and_nesting() {
    let (_dir, db) = setup_db();
    let (edited_id, jpeg_id, raw_id) = db
        .write(|conn| {
            add_folder(conn, "f-edited", "Edited Photos", "/photos/edited");
            add_folder(conn, "f-dump", "Photo Dump", "/photos/dump");

            let ts = 1_793_577_596_090i64;
            let raw_id = ingest_photo(
                conn,
                "f-dump",
                "2026/hikes/Bald Mountain 11-01/RAW/4K1A1899.CR3",
                "cr3",
                ts,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                None,
                Some(ts),
                6960,
                4640,
                ts,
                Some(0x1122_3344_5566_7788),
            );
            let jpeg_id = ingest_photo(
                conn,
                "f-dump",
                "2026/hikes/Bald Mountain 11-01/JPEG/4K1A1899.JPG",
                "jpg",
                ts,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                None,
                Some(ts),
                6960,
                4640,
                ts,
                Some(0x1122_3344_5566_7789),
            );
            // Completely renamed exported edit in a deeply nested different folder!
            let edited_id = ingest_photo(
                conn,
                "f-edited",
                "2026/hikes/Bald Mountain/Final/Alpenglow_Print_v2.jpg",
                "jpg",
                ts,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                Some("Adobe Photoshop Lightroom Classic 15.3 (Windows)"),
                Some(ts + 3_600_000),
                9363,
                6242,
                ts + 3_600_000,
                Some(0x1122_3344_5566_778A),
            );
            Ok((edited_id, jpeg_id, raw_id))
        })
        .await
        .unwrap();

    // 1. All Folders (collapsed): returns 1 tile — the Edited cover — with stack_count=3
    let (all_items, _, all_total) = db
        .read(|conn| list_assets(conn, &RbacScope::AdminAll, &AssetListQuery::default()))
        .await
        .unwrap();
    assert_eq!(all_total, 1);
    assert_eq!(all_items.len(), 1);
    assert_eq!(all_items[0].id, edited_id);
    assert_eq!(all_items[0].stack_count, 3);
    assert!(all_items[0].stack_has_edited);
    assert!(all_items[0].stack_has_raw);

    // 2. Unstacked view returns all 3
    let (unstacked, _, unstacked_total) = db
        .read(|conn| {
            list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    collapse_stacks: false,
                    ..Default::default()
                },
            )
        })
        .await
        .unwrap();
    assert_eq!(unstacked_total, 3);
    assert_eq!(unstacked.len(), 3);

    // 3. Filtering to Photo Dump collapses JPEG+RAW -> JPEG wins inside Photo Dump
    let (dump_items, _, dump_total) = db
        .read(|conn| {
            list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    folder_id: Some("f-dump".to_string()),
                    ..Default::default()
                },
            )
        })
        .await
        .unwrap();
    assert_eq!(dump_total, 1);
    assert_eq!(dump_items[0].id, jpeg_id);

    // 4. Drilling into nested RAW subfolder returns the RAW cleanly
    let (raw_sub_items, _, raw_sub_total) = db
        .read(|conn| {
            list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    folder_id: Some("f-dump".to_string()),
                    path_prefix: Some("2026/hikes/Bald Mountain 11-01/RAW".to_string()),
                    ..Default::default()
                },
            )
        })
        .await
        .unwrap();
    assert_eq!(raw_sub_total, 1);
    assert_eq!(raw_sub_items[0].id, raw_id);

    // 5. Lightbox detail on Edited includes all 3 stack members ordered Edited > JPEG > RAW
    let detail = db
        .read({
            let edited_id = edited_id.clone();
            move |conn| {
                truongphoto_db::repo::assets::get_asset_detail(
                    conn,
                    &RbacScope::AdminAll,
                    &edited_id,
                )
            }
        })
        .await
        .unwrap()
        .expect("detail exists");
    assert_eq!(detail.stack_members.len(), 3);
    assert_eq!(detail.stack_members[0].asset_id, edited_id);
    assert!(detail.stack_members[0].is_cover);
    assert_eq!(detail.stack_members[0].stack_role, "edited");
    assert_eq!(detail.stack_members[1].asset_id, jpeg_id);
    assert_eq!(detail.stack_members[1].stack_role, "jpeg");
    assert_eq!(detail.stack_members[2].asset_id, raw_id);
    assert_eq!(detail.stack_members[2].stack_role, "raw");
}

#[tokio::test]
async fn burst_same_second_disambiguated_by_subsec_and_dhash_and_never_stacks_two_raws() {
    let (_dir, db) = setup_db();
    let (ed_a, raw_a, raw_b, ed_no_sub_b) = db
        .write(|conn| {
            add_folder(conn, "f-edited", "Edited", "/edited");
            add_folder(conn, "f-raw", "Dump", "/dump");

            // Case A: Exact subsec burst separation (4K1A1896 at .090s vs 4K1A1897 at .400s)
            let sec1 = 1_793_577_560_000i64;
            let raw_a = ingest_photo(
                conn,
                "f-raw",
                "RAW/4K1A1896.CR3",
                "cr3",
                sec1 + 90,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                None,
                None,
                6960,
                4640,
                sec1,
                Some(0x0F0F_0F0F_0F0F_0F0F),
            );
            let _jpg_a = ingest_photo(
                conn,
                "f-raw",
                "JPEG/4K1A1896.JPG",
                "jpg",
                sec1 + 90,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                None,
                None,
                6960,
                4640,
                sec1,
                Some(0x0F0F_0F0F_0F0F_0F0E),
            );
            let ed_a = ingest_photo(
                conn,
                "f-edited",
                "0007_20261101-4K1A1896.jpg",
                "jpg",
                sec1 + 90,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                Some("Adobe Photoshop Lightroom Classic 15.3"),
                Some(sec1 + 9_000_000),
                6873,
                4582,
                sec1 + 9_000_000,
                Some(0x0F0F_0F0F_0F0F_0F0C),
            );
            let raw_b = ingest_photo(
                conn,
                "f-raw",
                "RAW/4K1A1897.CR3",
                "cr3",
                sec1 + 400,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(12.0),
                Some(4.0),
                Some(0.00625),
                Some(100),
                None,
                None,
                6960,
                4640,
                sec1,
                Some(0xF0F0_F0F0_F0F0_F0F0u64 as i64),
            );

            // Case B: Stripped SubSecTimeOriginal on burst — dHash disambiguates Frame D from Frame C!
            let sec2 = 1_793_580_000_000i64;
            let _raw_c = ingest_photo(
                conn,
                "f-raw",
                "RAW/IMG_0010.CR3",
                "cr3",
                sec2,
                false,
                None,
                Some("Canon EOS R7"),
                Some(50.0),
                Some(1.8),
                Some(0.002),
                Some(200),
                None,
                None,
                6960,
                4640,
                sec2,
                Some(0x0000_0000_FFFF_FFFF),
            );
            let _raw_d = ingest_photo(
                conn,
                "f-raw",
                "RAW/IMG_0011.CR3",
                "cr3",
                sec2,
                false,
                None,
                Some("Canon EOS R7"),
                Some(50.0),
                Some(1.8),
                Some(0.002),
                Some(200),
                None,
                None,
                6960,
                4640,
                sec2,
                Some(0xFFFF_FFFF_0000_0000u64 as i64),
            );
            // Renamed edit with stripped subsec, visually close to raw_d (1 bit away)
            let ed_no_sub_b = ingest_photo(
                conn,
                "f-edited",
                "Final_Portrait_Graded.jpg",
                "jpg",
                sec2,
                false,
                None,
                Some("Canon EOS R7"),
                Some(50.0),
                Some(1.8),
                Some(0.002),
                Some(200),
                Some("Capture One 23"),
                Some(sec2 + 500_000),
                6960,
                4640,
                sec2 + 500_000,
                Some(0xFFFF_FFFF_0000_0001u64 as i64),
            );

            Ok((ed_a, raw_a, raw_b, ed_no_sub_b))
        })
        .await
        .unwrap();

    // Collapsed timeline must have 4 distinct shots:
    // 1. ed_a (stack of 3: ed_a + jpg_a + raw_a)
    // 2. raw_b (standalone, NOT merged with raw_a!)
    // 3. raw_c (standalone, NOT merged with raw_d!)
    // 4. ed_no_sub_b (stack of 2: ed_no_sub_b + raw_d)
    let (items, _, total) = db
        .read(|conn| list_assets(conn, &RbacScope::AdminAll, &AssetListQuery::default()))
        .await
        .unwrap();
    assert_eq!(total, 4);
    let by_id: std::collections::HashMap<_, _> =
        items.into_iter().map(|i| (i.id.clone(), i)).collect();
    assert_eq!(by_id[&ed_a].stack_count, 3);
    assert_eq!(by_id[&raw_b].stack_count, 1);
    assert_eq!(by_id[&ed_no_sub_b].stack_count, 2);
    assert!(!by_id.contains_key(&raw_a));
}

#[tokio::test]
async fn timezone_mismatch_different_cameras_multiple_edits_and_rbac_isolation() {
    let (_dir, db) = setup_db();
    let (ed_bw, ed_color, raw_tz, sony_id) = db
        .write(|conn| {
            add_folder(conn, "f-edited", "Edited Photos", "/edited");
            add_folder(conn, "f-raw", "RAW Archive", "/raw");

            // RAW has OffsetTimeOriginal = -06:00 (-360m); local time 17:59:20 -> UTC = 23:59:20
            let utc_ms = 1_793_577_560_150i64;
            let wall_ms = utc_ms - 360 * 60_000;
            let raw_tz = ingest_photo(
                conn,
                "f-raw",
                "RAW/DSC0001.CR3",
                "cr3",
                utc_ms,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(24.0),
                Some(2.8),
                Some(0.004),
                Some(400),
                None,
                None,
                6960,
                4640,
                utc_ms,
                Some(0xAAAA_BBBB_CCCC_DDDDu64 as i64),
            );
            // Export 1 (Color): editor omitted OffsetTimeOriginal -> stored wall-clock time as naive UTC
            let ed_color = ingest_photo(
                conn,
                "f-edited",
                "Mountain_Color.jpg",
                "jpg",
                wall_ms,
                true,
                None,
                Some("Canon EOS R7"),
                Some(24.0),
                Some(2.8),
                Some(0.004),
                Some(400),
                Some("Darktable 4.8"),
                Some(utc_ms + 1_000_000),
                6960,
                4640,
                utc_ms + 1_000_000,
                Some(0xAAAA_BBBB_CCCC_DDDCu64 as i64),
            );
            // Export 2 (B&W): exported later -> becomes newest cover of the 3-item stack!
            let ed_bw = ingest_photo(
                conn,
                "f-edited",
                "Mountain_BW.jpg",
                "jpg",
                utc_ms,
                true,
                Some(-360),
                Some("Canon EOS R7"),
                Some(24.0),
                Some(2.8),
                Some(0.004),
                Some(400),
                Some("Adobe Photoshop Lightroom Classic 15.3"),
                Some(utc_ms + 5_000_000),
                6960,
                4640,
                utc_ms + 5_000_000,
                Some(0xAAAA_BBBB_CCCC_DDDEu64 as i64),
            );
            // Second photographer's Sony A7IV firing at the exact same millisecond -> MUST NOT stack!
            let sony_id = ingest_photo(
                conn,
                "f-raw",
                "RAW/SONY0001.ARW",
                "arw",
                utc_ms,
                true,
                Some(-360),
                Some("ILCE-7M4"),
                Some(24.0),
                Some(2.8),
                Some(0.004),
                Some(400),
                None,
                None,
                7008,
                4672,
                utc_ms,
                Some(0x1234_5678_9ABC_DEF0),
            );
            Ok((ed_bw, ed_color, raw_tz, sony_id))
        })
        .await
        .unwrap();

    // Admin sees 2 items: 3-item Canon stack (ed_bw on top) + standalone Sony shot
    let (admin_items, _, admin_total) = db
        .read(|conn| list_assets(conn, &RbacScope::AdminAll, &AssetListQuery::default()))
        .await
        .unwrap();
    assert_eq!(admin_total, 2);
    let by_id: std::collections::HashMap<_, _> =
        admin_items.into_iter().map(|i| (i.id.clone(), i)).collect();
    assert_eq!(by_id[&ed_bw].stack_count, 3);
    assert_eq!(by_id[&sony_id].stack_count, 1);
    assert!(!by_id.contains_key(&ed_color));
    assert!(!by_id.contains_key(&raw_tz));

    // Guest restricted ONLY to `f-edited`: sees stack_count=2 (only Color + B&W), NEVER sees RAW!
    let guest_scope = RbacScope::from_role_and_folders(Role::Viewer, vec!["f-edited".to_string()]);
    let (guest_items, _, guest_total) = db
        .read({
            let guest_scope = guest_scope.clone();
            move |conn| list_assets(conn, &guest_scope, &AssetListQuery::default())
        })
        .await
        .unwrap();
    assert_eq!(guest_total, 1);
    assert_eq!(guest_items[0].id, ed_bw);
    assert_eq!(guest_items[0].stack_count, 2);
    assert!(!guest_items[0].stack_has_raw);

    let guest_detail = db
        .read({
            let ed_bw = ed_bw.clone();
            move |conn| truongphoto_db::repo::assets::get_asset_detail(conn, &guest_scope, &ed_bw)
        })
        .await
        .unwrap()
        .expect("guest can see edited");
    assert_eq!(guest_detail.stack_members.len(), 2);
    assert!(guest_detail
        .stack_members
        .iter()
        .all(|m| m.folder_id == "f-edited"));
}

#[tokio::test]
async fn nested_subfolders_like_escaping_traversal_rejection_and_manual_stack_controls() {
    let (_dir, db) = setup_db();
    let (a1, a2, a3) = db
        .write(|conn| {
            add_folder(conn, "f1", "Library", "/lib");
            let a1 = ingest_photo(
                conn,
                "f1",
                "2026/hikes/100%_snow/raws/IMG_001.CR3",
                "cr3",
                1_790_000_001_000,
                true,
                None,
                Some("Canon EOS R7"),
                Some(35.0),
                Some(2.0),
                Some(0.001),
                Some(100),
                None,
                None,
                6960,
                4640,
                1_790_000_001_000,
                None,
            );
            let a2 = ingest_photo(
                conn,
                "f1",
                "2026/hikes/1000_snow/raws/IMG_002.CR3",
                "cr3",
                1_790_000_050_000,
                true,
                None,
                Some("Canon EOS R7"),
                Some(35.0),
                Some(2.0),
                Some(0.001),
                Some(100),
                None,
                None,
                6960,
                4640,
                1_790_000_050_000,
                None,
            );
            let a3 = ingest_photo(
                conn,
                "f1",
                "2026/hikes/100%_snow/jpegs/IMG_001.JPG",
                "jpg",
                1_790_000_001_000,
                true,
                None,
                Some("Canon EOS R7"),
                Some(35.0),
                Some(2.0),
                Some(0.001),
                Some(100),
                None,
                None,
                6960,
                4640,
                1_790_000_001_000,
                None,
            );
            Ok((a1, a2, a3))
        })
        .await
        .unwrap();

    // Subfolder tree lists every depth cleanly
    let subs = db
        .read(|conn| list_folder_subfolders(conn, &RbacScope::AdminAll, "f1"))
        .await
        .unwrap();
    let paths: Vec<_> = subs
        .iter()
        .map(|s| (s.path.as_str(), s.asset_count))
        .collect();
    assert!(paths.contains(&("2026", 3)));
    assert!(paths.contains(&("2026/hikes", 3)));
    assert!(paths.contains(&("2026/hikes/100%_snow", 2)));
    assert!(paths.contains(&("2026/hikes/100%_snow/raws", 1)));
    assert!(paths.contains(&("2026/hikes/100%_snow/jpegs", 1)));

    // `%` and `_` in path_prefix MUST NOT act as SQL LIKE wildcards matching `1000_snow`!
    let (snow_items, _, snow_total) = db
        .read(|conn| {
            list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    folder_id: Some("f1".to_string()),
                    path_prefix: Some("2026/hikes/100%_snow".to_string()),
                    collapse_stacks: false,
                    ..Default::default()
                },
            )
        })
        .await
        .unwrap();
    assert_eq!(snow_total, 2);
    assert!(snow_items.iter().all(|i| i.id != a2));

    // Traversal `..` must be rejected
    let traversal_res = db
        .read(|conn| {
            list_assets(
                conn,
                &RbacScope::AdminAll,
                &AssetListQuery {
                    folder_id: Some("f1".to_string()),
                    path_prefix: Some("../etc".to_string()),
                    ..Default::default()
                },
            )
        })
        .await;
    assert!(traversal_res.is_err());

    // Filtered timeline buckets respect folder & path_prefix
    let buckets = db
        .read(|conn| {
            list_timeline_buckets_filtered(
                conn,
                &RbacScope::AdminAll,
                Some("f1"),
                Some("2026/hikes/100%_snow"),
                true,
            )
        })
        .await
        .unwrap();
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].count, 1);

    // Manual stack, set cover, unstack survive reconcile_all_stacks!
    let manual_gid = db
        .write({
            let a1 = a1.clone();
            let a2 = a2.clone();
            let a3 = a3.clone();
            move |conn| {
                // First unstack a3 from a1's auto stack
                let auto_gid: Option<String> = conn
                    .query_row(
                        "SELECT stack_group_id FROM assets WHERE id = ?1",
                        rusqlite::params![&a1],
                        |r| r.get(0),
                    )
                    .unwrap();
                if let Some(gid) = auto_gid {
                    remove_from_stack(conn, &RbacScope::AdminAll, &gid, &a3)?;
                }
                // Manually stack a1 + a2 with a1 as cover
                let gid = create_manual_stack(
                    conn,
                    &RbacScope::AdminAll,
                    &[a1.clone(), a2.clone()],
                    Some(&a1),
                )?;
                set_stack_cover(conn, &RbacScope::AdminAll, &gid, &a2)?;
                let _ = reconcile_all_stacks(conn)?;
                Ok(gid)
            }
        })
        .await
        .unwrap();

    let (after_items, _, _) = db
        .read(|conn| list_assets(conn, &RbacScope::AdminAll, &AssetListQuery::default()))
        .await
        .unwrap();
    let cover = after_items
        .iter()
        .find(|i| i.stack_group_id.as_deref() == Some(manual_gid.as_str()));
    assert_eq!(cover.map(|c| c.id.as_str()), Some(a2.as_str()));
    // Unstacked a3 must remain standalone and not re-merge on reconcile_all_stacks
    let a3_item = after_items
        .iter()
        .find(|i| i.id == a3)
        .expect("a3 standalone");
    assert_eq!(a3_item.stack_count, 1);
    assert_eq!(a3_item.kind, AssetKind::Photo);
}
