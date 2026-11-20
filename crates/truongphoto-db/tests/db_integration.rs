use rusqlite::{params, Connection, OpenFlags};
use std::path::{Path, PathBuf};
use truongphoto_core::config::{AppSettings, StoragePaths};
use truongphoto_core::models::{ClientKind, JobStatus};
use truongphoto_core::rbac::{RbacScope, Role};
use truongphoto_db::repo::assets::AssetListQuery;
use truongphoto_db::repo::folders::{CreateFolder, UpdateFolder};
use truongphoto_db::repo::jobs::EnqueueJob;
use truongphoto_db::repo::sessions::CreateSession;
use truongphoto_db::repo::settings::NewAuditLog;
use truongphoto_db::repo::users::{CreateUser, UpdateUser};
use truongphoto_db::{repo, DbError, DbHandle};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tp-db-test-{}", uuid::Uuid::new_v4()));
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

fn insert_test_folder(conn: &Connection, id: &str, name: &str, root: &str) {
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

fn insert_test_asset(
    conn: &Connection,
    id: &str,
    folder_id: &str,
    kind: &str,
    capture_at_ms: i64,
    is_favorite: bool,
    deleted_at_ms: Option<i64>,
) {
    conn.execute(
        "INSERT INTO assets (
            id, folder_id, kind, capture_at_ms, is_favorite, is_hidden,
            deleted_at_ms, thumb_ready, ai_stage, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 0, 0, 1000, 1000)",
        params![
            id,
            folder_id,
            kind,
            capture_at_ms,
            i64::from(is_favorite),
            deleted_at_ms
        ],
    )
    .expect("insert asset");
}

#[tokio::test]
async fn idempotent_migrations_on_reopen() {
    let tmp = TempDir::new();
    let db_path = tmp.path().join("library.db");

    let db1 = DbHandle::open(&db_path).expect("open db first time");
    let count1: i64 = db1
        .read(
            |conn| Ok(conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?),
        )
        .await
        .expect("query schema_migrations");
    assert_eq!(count1, 4);
    assert!(db1.db_size_bytes() > 0);
    db1.shutdown().await.expect("shutdown db1");

    let db2 = DbHandle::open(&db_path).expect("reopen db");
    let count2: i64 = db2
        .read(
            |conn| Ok(conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?),
        )
        .await
        .expect("query schema_migrations after reopen");
    assert_eq!(count2, 4);
    db2.shutdown().await.expect("shutdown db2");
}

#[tokio::test]
async fn v_assets_effective_coalesce_priority() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        insert_test_folder(conn, "f1", "Photos", "/tmp/f1");
        insert_test_asset(conn, "a-photo", "f1", "photo", 1000, false, None);
        insert_test_asset(conn, "a-video", "f1", "video", 2000, false, None);

        conn.execute(
            "INSERT INTO deterministic_photo_meta (
                asset_id, camera_model, gps_lat, gps_lon, geo_city, geo_country, taken_at_ms
             ) VALUES ('a-photo', 'Canon R5', 10.0, 20.0, 'Paris', 'France', 1500)",
            [],
        )?;
        conn.execute(
            "INSERT INTO deterministic_video_meta (
                asset_id, duration_ms, video_codec, gps_lat, gps_lon, geo_city, geo_country, taken_at_ms
             ) VALUES ('a-video', 9500, 'hevc', 30.0, 40.0, 'Tokyo', 'Japan', 2500)",
            [],
        )?;

        // Override only a-photo capture_at_ms, gps, city, title, rating
        conn.execute(
            "INSERT INTO user_overrides (
                asset_id, title, capture_at_ms, gps_lat, gps_lon, geo_city, rating, updated_at_ms
             ) VALUES ('a-photo', 'Custom Title', 1900, 11.1, 22.2, 'Lyon', 5, 9999)",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("seed assets and overrides");

    let (items, next, total) = db
        .read(|conn| {
            repo::assets::list_assets(conn, &RbacScope::AdminAll, &AssetListQuery::default())
        })
        .await
        .expect("list_assets");

    assert_eq!(total, 2);
    assert!(next.is_none());
    // a-video has effective_capture_at_ms = 2500 > a-photo's overridden 1900
    assert_eq!(items[0].id, "a-video");
    assert_eq!(items[0].capture_at_ms, 2500);
    assert_eq!(items[0].duration_ms, Some(9500));
    assert_eq!(items[0].geo_city.as_deref(), Some("Tokyo"));

    assert_eq!(items[1].id, "a-photo");
    assert_eq!(items[1].capture_at_ms, 1900); // user_overrides > deterministic_photo_meta (1500) > assets (1000)
    assert_eq!(items[1].title.as_deref(), Some("Custom Title"));
    assert_eq!(items[1].rating, Some(5));
    assert_eq!(items[1].gps_lat, Some(11.1));
    assert_eq!(items[1].gps_lon, Some(22.2));
    assert_eq!(items[1].geo_city.as_deref(), Some("Lyon"));
    assert_eq!(items[1].geo_country.as_deref(), Some("France")); // falls back to deterministic_photo_meta
    assert_eq!(items[1].camera_model.as_deref(), Some("Canon R5"));

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn ocr_fts_triggers_sync_insert_update_delete() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        insert_test_folder(conn, "f1", "Docs", "/tmp/docs");
        insert_test_asset(conn, "a1", "f1", "photo", 1000, false, None);
        conn.execute(
            "INSERT INTO ocr_documents (asset_id, full_text, lang_hint, model_id, updated_at_ms)
             VALUES ('a1', 'Golden Gate Bridge San Francisco', 'en', 'paddle-v4', 1000)",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("insert ocr doc");

    let hits: Vec<String> = db
        .read(|conn| {
            let mut stmt =
                conn.prepare("SELECT asset_id FROM ocr_fts WHERE ocr_fts MATCH 'Golden'")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .expect("query fts after insert");
    assert_eq!(hits, vec!["a1"]);

    db.write(|conn| {
        conn.execute(
            "UPDATE ocr_documents SET full_text = 'Eiffel Tower Paris', updated_at_ms = 2000 WHERE asset_id = 'a1'",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("update ocr doc");

    let (old_hits, new_hits): (Vec<String>, Vec<String>) = db
        .read(|conn| {
            let mut s1 =
                conn.prepare("SELECT asset_id FROM ocr_fts WHERE ocr_fts MATCH 'Golden'")?;
            let old = s1
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let mut s2 =
                conn.prepare("SELECT asset_id FROM ocr_fts WHERE ocr_fts MATCH 'Eiffel'")?;
            let new = s2
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok((old, new))
        })
        .await
        .expect("query fts after update");
    assert!(old_hits.is_empty());
    assert_eq!(new_hits, vec!["a1"]);

    db.write(|conn| {
        conn.execute("DELETE FROM ocr_documents WHERE asset_id = 'a1'", [])?;
        Ok(())
    })
    .await
    .expect("delete ocr doc");

    let after_del: Vec<String> = db
        .read(|conn| {
            let mut stmt =
                conn.prepare("SELECT asset_id FROM ocr_fts WHERE ocr_fts MATCH 'Eiffel'")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .expect("query fts after delete");
    assert!(after_del.is_empty());

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn single_writer_concurrency_and_savepoint_isolation() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        insert_test_folder(conn, "f1", "Stress", "/tmp/stress");
        Ok(())
    })
    .await
    .expect("create stress folder");

    // 16 concurrent writers submitting 200 batched writes total (~12-13 per task) + 16 concurrent readers
    let total_writes = 200usize;
    let num_writers = 16usize;
    let mut writer_handles = Vec::with_capacity(num_writers);

    for worker in 0..num_writers {
        let db_w = db.clone();
        writer_handles.push(tokio::spawn(async move {
            for idx in 0..total_writes {
                if idx % num_writers != worker {
                    continue;
                }
                let asset_id = format!("stress-{idx:04}");
                let ts = 1_700_000_000_000i64 + idx as i64;
                db_w.write_batched(move |tx| {
                    tx.execute(
                        "INSERT INTO assets (
                            id, folder_id, kind, capture_at_ms, is_favorite, is_hidden,
                            thumb_ready, ai_stage, created_at_ms, updated_at_ms
                         ) VALUES (?1, 'f1', 'photo', ?2, 0, 0, 0, 0, 1000, 1000)",
                        params![asset_id, ts],
                    )?;
                    Ok(())
                })
                .await
                .expect("batched write succeeds without SQLITE_BUSY");
            }
        }));
    }

    let mut reader_handles = Vec::with_capacity(16);
    for _ in 0..16 {
        let db_r = db.clone();
        reader_handles.push(tokio::spawn(async move {
            for _ in 0..25 {
                let _count = db_r
                    .read(|conn| repo::assets::count_active_assets(conn, &RbacScope::AdminAll))
                    .await
                    .expect("concurrent read succeeds without SQLITE_BUSY");
                tokio::task::yield_now().await;
            }
        }));
    }

    for h in writer_handles {
        h.await.expect("writer task join");
    }
    for h in reader_handles {
        h.await.expect("reader task join");
    }

    let final_count = db
        .read(|conn| repo::assets::count_active_assets(conn, &RbacScope::AdminAll))
        .await
        .expect("count stress assets");
    assert_eq!(final_count, total_writes as u64);

    // Verify SAVEPOINT isolation: one failing item in a batch does not roll back valid siblings.
    let ok_db = db.clone();
    let bad_db = db.clone();
    let (res_ok, res_bad) = tokio::join!(
        ok_db.write_batched(|tx| {
            tx.execute(
                "INSERT INTO assets (
                    id, folder_id, kind, capture_at_ms, is_favorite, is_hidden,
                    thumb_ready, ai_stage, created_at_ms, updated_at_ms
                 ) VALUES ('batch-valid', 'f1', 'photo', 9999, 0, 0, 0, 0, 1000, 1000)",
                [],
            )?;
            Ok(())
        }),
        bad_db.write_batched(|tx| {
            tx.execute(
                "INSERT INTO assets (
                    id, folder_id, kind, capture_at_ms, is_favorite, is_hidden,
                    thumb_ready, ai_stage, created_at_ms, updated_at_ms
                 ) VALUES ('batch-invalid', 'nonexistent-folder', 'photo', 9999, 0, 0, 0, 0, 1000, 1000)",
                [],
            )?;
            Ok(())
        })
    );
    assert!(res_ok.is_ok());
    assert!(res_bad.is_err());

    let after_savepoint_count = db
        .read(|conn| repo::assets::count_active_assets(conn, &RbacScope::AdminAll))
        .await
        .expect("count after savepoint test");
    assert_eq!(after_savepoint_count, (total_writes + 1) as u64);

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn read_pool_rejects_writes_query_only() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    let err = db
        .read(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value_json, updated_at_ms) VALUES ('k', '1', 1000)",
                [],
            )?;
            Ok(())
        })
        .await
        .expect_err("ReadPool must reject write SQL due to PRAGMA query_only = ON");

    assert!(matches!(err, DbError::Sqlite(_)));
    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn rolling_vacuum_into_backup_and_pruning() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");
    let backup_dir = tmp.path().join("backups");

    db.write(|conn| {
        repo::settings::set_setting(conn, "backup_marker", &"hello-backup")?;
        Ok(())
    })
    .await
    .expect("seed setting");

    let b1 = db.backup_to_dir(&backup_dir, 2).await.expect("backup 1");
    assert!(b1.path.exists());
    assert!(b1.size_bytes > 0);
    assert_eq!(b1.pruned_files, 0);

    let b2 = db.backup_to_dir(&backup_dir, 2).await.expect("backup 2");
    assert_eq!(b2.pruned_files, 0);

    let b3 = db.backup_to_dir(&backup_dir, 2).await.expect("backup 3");
    assert_eq!(b3.pruned_files, 1);

    // Verify only 2 backups remain and newest is readable SQLite DB containing our setting.
    let remaining: Vec<_> = std::fs::read_dir(&backup_dir)
        .expect("read backup dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.starts_with("library-") && s.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(remaining.len(), 2);
    assert!(!b1.path.exists());
    assert!(b3.path.exists());

    let ro_conn = Connection::open_with_flags(&b3.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open backup snapshot");
    let marker = repo::settings::get_setting::<String>(&ro_conn, "backup_marker")
        .expect("read backup setting");
    assert_eq!(marker.as_deref(), Some("hello-backup"));

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn strict_rbac_people_query_prevents_folder_leakage() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        insert_test_folder(conn, "f1", "Family", "/tmp/f1");
        insert_test_folder(conn, "f2", "Private", "/tmp/f2");

        insert_test_asset(conn, "a1", "f1", "photo", 1000, false, None);
        insert_test_asset(conn, "a2", "f1", "photo", 1100, false, None);
        insert_test_asset(conn, "a3", "f2", "photo", 1200, false, None);
        insert_test_asset(conn, "a4", "f2", "photo", 1300, false, None);

        // Person A has faces in Folder 1 (face-a1 det 0.70, face-a2 det 0.92) AND Folder 2 (face-a3 det 0.99).
        // Admin pins Person A's cover_face_id to face-a3 (which lives in Folder 2!).
        conn.execute(
            "INSERT INTO people (id, name, is_hidden, is_pinned, cover_face_id, created_at_ms, updated_at_ms)
             VALUES ('person-a', 'Alice', 0, 1, 'face-a3', 1000, 1000)",
            [],
        )?;
        // Person B ONLY has a face in Folder 2.
        conn.execute(
            "INSERT INTO people (id, name, is_hidden, is_pinned, cover_face_id, created_at_ms, updated_at_ms)
             VALUES ('person-b', 'Bob', 0, 0, 'face-b1', 1000, 1000)",
            [],
        )?;

        conn.execute(
            "INSERT INTO faces (id, asset_id, person_id, bbox_x, bbox_y, bbox_w, bbox_h, det_score, assigned_by, created_at_ms, updated_at_ms)
             VALUES
             ('face-a1', 'a1', 'person-a', 0.1, 0.1, 0.2, 0.2, 0.70, 'manual', 1000, 1000),
             ('face-a2', 'a2', 'person-a', 0.1, 0.1, 0.2, 0.2, 0.92, 'manual', 1001, 1001),
             ('face-a3', 'a3', 'person-a', 0.1, 0.1, 0.2, 0.2, 0.99, 'manual', 1002, 1002),
             ('face-b1', 'a4', 'person-b', 0.2, 0.2, 0.3, 0.3, 0.95, 'manual', 1003, 1003)",
            [],
        )?;
        Ok(())
    })
    .await
    .expect("seed people and faces");

    // Admin sees both Person A (3 faces, pinned cover face-a3) and Person B (1 face).
    let admin_people = db
        .read(|conn| repo::people::list_people(conn, &RbacScope::AdminAll))
        .await
        .expect("admin list_people");
    assert_eq!(admin_people.len(), 2);
    assert_eq!(admin_people[0].id, "person-a");
    assert_eq!(admin_people[0].face_count, 3);
    assert_eq!(admin_people[0].cover_face_id.as_deref(), Some("face-a3"));
    assert_eq!(admin_people[1].id, "person-b");
    assert_eq!(admin_people[1].face_count, 1);

    // Viewer with ACL [f1] sees ONLY Person A, face_count == 2, and cover_face_id falls back
    // to highest-det_score face strictly inside f1 ('face-a2'), NEVER leaking 'face-a3' from f2!
    let viewer_scope = RbacScope::RestrictedFolders(vec!["f1".to_string()]);
    let viewer_people = db
        .read(move |conn| repo::people::list_people(conn, &viewer_scope))
        .await
        .expect("viewer list_people");
    assert_eq!(viewer_people.len(), 1);
    assert_eq!(viewer_people[0].id, "person-a");
    assert_eq!(viewer_people[0].face_count, 2);
    assert_eq!(viewer_people[0].cover_face_id.as_deref(), Some("face-a2"));

    // Empty ACL sees zero people (fail-closed).
    let empty_people = db
        .read(|conn| repo::people::list_people(conn, &RbacScope::RestrictedFolders(vec![])))
        .await
        .expect("empty scope list_people");
    assert!(empty_people.is_empty());

    db.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn repos_crud_and_rbac_smoke() {
    let tmp = TempDir::new();
    let db = DbHandle::open(tmp.path().join("library.db")).expect("open db");

    db.write(|conn| {
        // Users & ACLs
        let admin = repo::users::create_user(
            conn,
            &CreateUser {
                id: "u-admin".to_string(),
                username: "Ian".to_string(),
                display_name: "Ian Truong".to_string(),
                role: Role::Admin,
                pin_hash: "phc-admin".to_string(),
                avatar_color: "#4a4a4a".to_string(),
            },
        )?;
        assert_eq!(admin.role, Role::Admin);

        let viewer = repo::users::create_user(
            conn,
            &CreateUser {
                id: "u-viewer".to_string(),
                username: "mom".to_string(),
                display_name: "Mom".to_string(),
                role: Role::Viewer,
                pin_hash: "phc-viewer".to_string(),
                avatar_color: "#556b5d".to_string(),
            },
        )?;
        assert_eq!(viewer.role, Role::Viewer);

        assert!(repo::users::get_user_by_username(conn, "IAN")?.is_some());
        assert_eq!(
            repo::users::get_primary_active_admin(conn)?
                .expect("primary admin")
                .id,
            "u-admin"
        );
        assert_eq!(repo::users::count_active_admins(conn)?, 1);

        // Folders
        repo::folders::create_folder(
            conn,
            &CreateFolder {
                id: "f1".to_string(),
                name: "Shared".to_string(),
                root_path: "/tmp/shared".to_string(),
                volume_uuid: Some("VOL-1".to_string()),
                volume_label: Some("MacHD".to_string()),
                is_managed_uploads: true,
                allow_disk_delete: false,
            },
        )?;
        repo::folders::create_folder(
            conn,
            &CreateFolder {
                id: "f2".to_string(),
                name: "AdminOnly".to_string(),
                root_path: "/tmp/admin".to_string(),
                volume_uuid: None,
                volume_label: None,
                is_managed_uploads: false,
                allow_disk_delete: false,
            },
        )?;
        repo::folders::update_folder(
            conn,
            "f2",
            &UpdateFolder {
                allow_disk_delete: Some(true),
                ..Default::default()
            },
        )?;
        assert_eq!(repo::folders::count_folders(conn)?, 2);

        // Replace ACLs atomically
        repo::users::replace_user_folder_acls(
            conn,
            "u-viewer",
            &["f1".to_string(), "f1".to_string()],
        )?;
        assert_eq!(
            repo::users::get_allowed_folder_ids(conn, "u-viewer")?,
            vec!["f1".to_string()]
        );

        let all_users = repo::users::list_users_with_acls(conn)?;
        assert_eq!(all_users.len(), 2);
        assert_eq!(all_users[1].allowed_folder_ids, vec!["f1".to_string()]);
        assert_eq!(repo::users::list_active_profiles(conn)?.len(), 2);

        repo::users::update_user(
            conn,
            "u-viewer",
            &UpdateUser {
                display_name: Some("Mom Updated".to_string()),
                ..Default::default()
            },
        )?;

        // Sessions
        let now = 1_700_000_000_000i64;
        let sess = repo::sessions::create_session(
            conn,
            &CreateSession {
                id: "s1".to_string(),
                token_hash: "hash1".to_string(),
                user_id: "u-admin".to_string(),
                client_kind: ClientKind::Web,
                user_agent: Some("Safari".to_string()),
                ip_addr: Some("127.0.0.1".to_string()),
                expires_at_ms: now + 60_000,
            },
        )?;
        assert!(repo::sessions::find_active_session_by_token_hash(conn, "hash1", now)?.is_some());
        repo::sessions::touch_session(conn, &sess.id, now + 1000)?;
        assert!(repo::sessions::revoke_session_by_token_hash(conn, "hash1", now + 2000)?);
        assert!(repo::sessions::find_active_session_by_token_hash(conn, "hash1", now + 2000)?.is_none());
        assert_eq!(repo::sessions::prune_expired_sessions(conn, now + 2000)?, 1);

        // Assets, Buckets, Trash, Albums
        // 2026-11-15T00:00:00Z = 1794700800000 ms
        insert_test_asset(conn, "a1", "f1", "photo", 1_794_700_800_000, true, None);
        insert_test_asset(conn, "a2", "f2", "photo", 1_794_700_900_000, false, None);
        insert_test_asset(conn, "a3", "f1", "photo", 1_794_700_700_000, false, Some(1_794_701_000_000));

        conn.execute(
            "INSERT INTO albums (id, owner_user_id, title, cover_asset_id, created_at_ms, updated_at_ms)
             VALUES ('alb-1', 'u-admin', 'Trip', 'a2', 1000, 1000)",
            [],
        )?;
        conn.execute(
            "INSERT INTO album_assets (album_id, asset_id, position, added_at_ms)
             VALUES ('alb-1', 'a1', 0, 1000), ('alb-1', 'a2', 1, 1000)",
            [],
        )?;
        conn.execute(
            "INSERT INTO album_shares (album_id, shared_with_user_id, permission, created_at_ms)
             VALUES ('alb-1', 'u-viewer', 'view', 1000)",
            [],
        )?;

        // Jobs
        let job = repo::jobs::enqueue_job(
            conn,
            &EnqueueJob {
                kind: "folder_scan".to_string(),
                payload_json: r#"{"folder_id":"f1"}"#.to_string(),
                priority: 10,
                max_attempts: 2,
                run_after_ms: 0,
            },
        )?;
        assert_eq!(repo::jobs::count_active_jobs(conn)?, 1);
        let claimed = repo::jobs::claim_next_job(conn, Some(&["folder_scan"]), 1000)?
            .expect("claimed job");
        assert_eq!(claimed.id, job.id);
        assert_eq!(claimed.status, JobStatus::Running);
        let failed = repo::jobs::fail_job(conn, job.id, "retry me", Some(500), 1000)?
            .expect("requeued job");
        assert_eq!(failed.status, JobStatus::Queued);
        let claimed2 = repo::jobs::claim_next_job(conn, None, 2000)?.expect("re-claimed job");
        assert!(repo::jobs::complete_job(conn, claimed2.id, 3000)?);
        assert_eq!(repo::jobs::count_active_jobs(conn)?, 0);
        assert_eq!(repo::jobs::list_jobs(conn, Some(JobStatus::Completed), 10)?.len(), 1);

        // Settings & Audit Log
        let paths = StoragePaths::from_data_dir(PathBuf::from("/tmp/tp"));
        let mut settings = AppSettings::from_paths(&paths);
        settings.desktop_pin_lock = true;
        repo::settings::save_app_settings(conn, &settings)?;
        assert!(repo::settings::get_desktop_pin_lock(conn)?);
        assert!(!repo::settings::is_setup_completed(conn)?);
        repo::settings::set_setup_completed(conn, true)?;
        assert!(repo::settings::is_setup_completed(conn)?);
        repo::settings::set_server_pin_hash(conn, "phc-server")?;
        assert_eq!(
            repo::settings::get_server_pin_hash(conn)?.as_deref(),
            Some("phc-server")
        );
        let audit_id = repo::settings::insert_audit_log(
            conn,
            &NewAuditLog {
                actor_user_id: Some("u-admin"),
                action: "setup.completed",
                target_type: Some("server"),
                target_id: None,
                detail_json: "{}",
                ip_addr: Some("127.0.0.1"),
            },
        )?;
        assert!(audit_id > 0);

        Ok(())
    })
    .await
    .expect("seed CRUD smoke");

    // Verify viewer RBAC views
    let viewer_scope = RbacScope::RestrictedFolders(vec!["f1".to_string()]);
    let (v_folders, v_assets, v_buckets, v_trash, v_albums) = db
        .read(move |conn| {
            let folders = repo::folders::list_folders(conn, &viewer_scope)?;
            let assets =
                repo::assets::list_assets(conn, &viewer_scope, &AssetListQuery::default())?;
            let buckets = repo::assets::list_timeline_buckets(conn, &viewer_scope)?;
            let trash = repo::assets::list_trash(conn, &viewer_scope)?;
            let albums = repo::albums::list_albums(conn, "u-viewer", &viewer_scope)?;
            Ok((folders, assets, buckets, trash, albums))
        })
        .await
        .expect("viewer RBAC queries");

    assert_eq!(v_folders.len(), 1);
    assert_eq!(v_folders[0].id, "f1");
    assert_eq!(v_folders[0].asset_count, 1);
    assert_eq!(v_assets.0.len(), 1);
    assert_eq!(v_assets.0[0].id, "a1");
    assert_eq!(v_buckets.len(), 1);
    assert_eq!(v_buckets[0].year, 2026);
    assert_eq!(v_buckets[0].month, 11);
    assert_eq!(v_buckets[0].count, 1);
    assert_eq!(v_trash.0.len(), 1);
    assert_eq!(v_trash.0[0].id, "a3");
    assert_eq!(v_albums.len(), 1);
    assert_eq!(v_albums[0].asset_count, 1);
    assert!(v_albums[0].cover_asset_id.is_none()); // 'a2' is in f2, hidden from viewer!

    db.shutdown().await.expect("shutdown");
}
