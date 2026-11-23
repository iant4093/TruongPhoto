use chrono::{TimeZone, Utc};
use serde_json::Value;
use std::time::Duration;
use tempfile::tempdir;
use truongphoto_ai::chat::{
    merge_router_plan, parse_and_validate_router_plan, ChatTurnInput, ClassicChatRouter,
    DeterministicTestChatRouter, RerankCandidate, SidecarLlamaRouter, ROUTER_PLAN_JSON_SCHEMA,
};
use truongphoto_ai::models::{find_model_spec, MODEL_MANIFEST};
use truongphoto_ai::{AiError, AiPipeline, ChatRouterBackend};
use truongphoto_core::{AssetKind, ChatRole, ChatTier, RouterPlan};

fn fixed_date() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap()
}

fn turn_input(msg: &str, prior: RouterPlan, people: &[&str]) -> ChatTurnInput {
    ChatTurnInput {
        user_message: msg.to_string(),
        prior_filters: prior,
        recent_turns: Vec::new(),
        known_person_names: people.iter().map(|s| (*s).to_string()).collect(),
        known_cameras: Vec::new(),
        known_lenses: Vec::new(),
        library: Default::default(),
        current_date_utc: fixed_date(),
    }
}

#[test]
fn classic_router_multi_turn_refinement_and_clear() {
    let router = ClassicChatRouter::new();
    let people = ["Mom", "Alice"];

    // Turn 1: "beach photos in Tokyo"
    let t1 = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("beach photos in Tokyo", RouterPlan::default(), &people),
        )
        .unwrap();
    assert_eq!(t1.semantic_query.as_deref(), Some("beach"));
    assert_eq!(t1.location_query.as_deref(), Some("Tokyo"));
    assert_eq!(t1.kind, None);
    assert_eq!(t1.favorite, None);
    assert!(!t1.clear_prior_filters);

    // Turn 2: "only videos" -> carries forward beach + Tokyo, updates kind to Video
    let t2 = router
        .route_turn(ChatTier::Tier0, &turn_input("only videos", t1, &people))
        .unwrap();
    assert_eq!(t2.semantic_query.as_deref(), Some("beach"));
    assert_eq!(t2.location_query.as_deref(), Some("Tokyo"));
    assert_eq!(t2.kind, Some(AssetKind::Video));
    assert!(!t2.clear_prior_filters);

    // Turn 3: "favorites" -> carries forward beach + Tokyo + Video, adds favorite = Some(true)
    let t3 = router
        .route_turn(ChatTier::Tier0, &turn_input("favorites", t2, &people))
        .unwrap();
    assert_eq!(t3.semantic_query.as_deref(), Some("beach"));
    assert_eq!(t3.location_query.as_deref(), Some("Tokyo"));
    assert_eq!(t3.kind, Some(AssetKind::Video));
    assert_eq!(t3.favorite, Some(true));

    // Turn 4: "with mom" -> case-insensitive RBAC person match preserving canonical "Mom"
    let t4 = router
        .route_turn(ChatTier::Tier0, &turn_input("with mom", t3, &people))
        .unwrap();
    assert_eq!(t4.semantic_query.as_deref(), Some("beach"));
    assert_eq!(t4.location_query.as_deref(), Some("Tokyo"));
    assert_eq!(t4.kind, Some(AssetKind::Video));
    assert_eq!(t4.favorite, Some(true));
    assert_eq!(t4.person_names, vec!["Mom".to_string()]);

    // Turn 5: "start over" -> clears all carried-forward filters
    let t5 = router
        .route_turn(ChatTier::Tier0, &turn_input("start over", t4, &people))
        .unwrap();
    assert!(t5.clear_prior_filters);
    assert_eq!(t5.semantic_query, None);
    assert_eq!(t5.location_query, None);
    assert_eq!(t5.kind, None);
    assert_eq!(t5.favorite, None);
    assert!(t5.person_names.is_empty());

    // Verify other clear triggers too.
    for trigger in ["reset filters", "clear filters", "new search"] {
        let cleared = router
            .route_turn(
                ChatTier::Tier0,
                &turn_input(
                    trigger,
                    RouterPlan {
                        location_query: Some("Paris".to_string()),
                        ..Default::default()
                    },
                    &people,
                ),
            )
            .unwrap();
        assert!(cleared.clear_prior_filters, "trigger `{trigger}` failed");
        assert_eq!(cleared.location_query, None);
    }
}

#[test]
fn classic_router_dates_cameras_tags_ocr_and_media_kinds() {
    let router = ClassicChatRouter::new();

    // Relative dates anchored to 2026-11-20.
    let winter = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("videos from last winter", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(winter.kind, Some(AssetKind::Video));
    assert_eq!(
        winter.date_from_ms,
        Some(
            Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    assert_eq!(
        winter.date_to_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 2, 28, 23, 59, 59)
                .unwrap()
                .timestamp_millis()
                + 999
        )
    );
    assert_eq!(winter.location_query, None);

    let summer = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("last summer", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(
        summer.date_from_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    assert_eq!(
        summer.date_to_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 8, 31, 23, 59, 59)
                .unwrap()
                .timestamp_millis()
                + 999
        )
    );

    let last_month = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("last month", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(
        last_month.date_from_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    assert_eq!(
        last_month.date_to_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 10, 31, 23, 59, 59)
                .unwrap()
                .timestamp_millis()
                + 999
        )
    );

    let yesterday = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("yesterday", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(
        yesterday.date_from_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 11, 19, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    assert_eq!(
        yesterday.date_to_ms,
        Some(
            Utc.with_ymd_and_hms(2026, 11, 19, 23, 59, 59)
                .unwrap()
                .timestamp_millis()
                + 999
        )
    );

    let march_2025 = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("March 2025", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(
        march_2025.date_from_ms,
        Some(
            Utc.with_ymd_and_hms(2025, 3, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    assert_eq!(
        march_2025.date_to_ms,
        Some(
            Utc.with_ymd_and_hms(2025, 3, 31, 23, 59, 59)
                .unwrap()
                .timestamp_millis()
                + 999
        )
    );

    let in_2024 = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("in 2024", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(in_2024.location_query, None);
    assert_eq!(
        in_2024.date_from_ms,
        Some(
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        )
    );
    assert_eq!(
        in_2024.date_to_ms,
        Some(
            Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59)
                .unwrap()
                .timestamp_millis()
                + 999
        )
    );

    // Media kinds
    assert_eq!(
        router
            .route_turn(
                ChatTier::Tier0,
                &turn_input("live photos", RouterPlan::default(), &[])
            )
            .unwrap()
            .kind,
        Some(AssetKind::LivePhoto)
    );
    assert_eq!(
        router
            .route_turn(
                ChatTier::Tier0,
                &turn_input("motion photos", RouterPlan::default(), &[])
            )
            .unwrap()
            .kind,
        Some(AssetKind::MotionPhoto)
    );
    assert_eq!(
        router
            .route_turn(
                ChatTier::Tier0,
                &turn_input("raw", RouterPlan::default(), &[])
            )
            .unwrap()
            .kind,
        Some(AssetKind::RawJpgStack)
    );

    // Camera + Location + Tag
    let cam = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input(
                "shot on Fujifilm X100VI in Kyoto tagged sunset",
                RouterPlan::default(),
                &[],
            ),
        )
        .unwrap();
    assert_eq!(cam.camera_model.as_deref(), Some("Fujifilm X100VI"));
    assert_eq!(cam.location_query.as_deref(), Some("Kyoto"));
    assert_eq!(cam.tags, vec!["sunset".to_string()]);

    // OCR
    let receipt = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input("receipt in 2024", RouterPlan::default(), &[]),
        )
        .unwrap();
    assert_eq!(receipt.ocr_terms.as_deref(), Some("receipt"));
    assert!(receipt.date_from_ms.is_some());

    let text_saying = router
        .route_turn(
            ChatTier::Tier0,
            &turn_input(
                "photos with text saying \"Coffee House\" in Tokyo",
                RouterPlan::default(),
                &[],
            ),
        )
        .unwrap();
    assert_eq!(text_saying.ocr_terms.as_deref(), Some("Coffee House"));
    assert_eq!(text_saying.location_query.as_deref(), Some("Tokyo"));
    assert_eq!(text_saying.kind, None);
}

#[test]
fn router_plan_json_schema_validation_and_rejection() {
    let schema: Value = serde_json::from_str(ROUTER_PLAN_JSON_SCHEMA).unwrap();
    assert_eq!(schema["additionalProperties"], Value::Bool(false));

    // Valid structured plan parses cleanly.
    let valid_json = r#"{
        "semantic_query": "mountain hike",
        "ocr_terms": null,
        "person_names": ["Mom"],
        "location_query": "Switzerland",
        "camera_model": null,
        "tags": ["hiking"],
        "kind": "photo",
        "favorite": true,
        "date_from_ms": 1700000000000,
        "date_to_ms": 1705000000000,
        "clear_prior_filters": false,
        "assistant_reply": "Here are your favorite hiking photos from Switzerland.",
        "follow_up_suggestions": ["Only videos", "Start over"]
    }"#;
    let parsed = parse_and_validate_router_plan(valid_json).unwrap();
    assert_eq!(parsed.semantic_query.as_deref(), Some("mountain hike"));
    assert_eq!(parsed.person_names, vec!["Mom".to_string()]);
    assert_eq!(parsed.location_query.as_deref(), Some("Switzerland"));
    assert_eq!(parsed.kind, Some(AssetKind::Photo));
    assert_eq!(parsed.favorite, Some(true));

    // Rejects hallucinated unknown key `file_paths`.
    let hallucinated = r#"{
        "semantic_query": "sunset",
        "file_paths": ["/etc/passwd"],
        "assistant_reply": "Found files"
    }"#;
    let err = parse_and_validate_router_plan(hallucinated).unwrap_err();
    assert!(matches!(err, AiError::InvalidInput(_)));

    // Rejects malformed JSON.
    let malformed = "{not_valid_json";
    assert!(matches!(
        parse_and_validate_router_plan(malformed),
        Err(AiError::InvalidInput(_))
    ));

    // Rejects non-object JSON.
    assert!(matches!(
        parse_and_validate_router_plan("[]"),
        Err(AiError::InvalidInput(_))
    ));

    // Rejects inverted date range.
    let bad_dates = r#"{
        "date_from_ms": 2000,
        "date_to_ms": 1000,
        "assistant_reply": "bad dates"
    }"#;
    assert!(matches!(
        parse_and_validate_router_plan(bad_dates),
        Err(AiError::InvalidInput(_))
    ));

    // Verify merge_router_plan overlay + clear semantics.
    let merged = merge_router_plan(
        &parsed,
        &RouterPlan {
            kind: Some(AssetKind::Video),
            assistant_reply: "Switched to videos.".to_string(),
            ..Default::default()
        },
    );
    assert_eq!(merged.location_query.as_deref(), Some("Switzerland"));
    assert_eq!(merged.kind, Some(AssetKind::Video));
    assert!(!merged.clear_prior_filters);

    let cleared = merge_router_plan(
        &merged,
        &RouterPlan {
            clear_prior_filters: true,
            semantic_query: Some("cats".to_string()),
            assistant_reply: "Showing cats instead.".to_string(),
            ..Default::default()
        },
    );
    assert!(cleared.clear_prior_filters);
    assert_eq!(cleared.semantic_query.as_deref(), Some("cats"));
    assert_eq!(cleared.location_query, None);
    assert_eq!(cleared.kind, None);
}

#[test]
fn deterministic_test_router_tier3_rerank_and_idle_eviction() {
    let pipeline = AiPipeline::deterministic_test();
    assert!(!pipeline.chat.is_loaded());

    // Tier 0 leaves model unloaded.
    let input = ChatTurnInput {
        user_message: "sunset beach".to_string(),
        prior_filters: RouterPlan::default(),
        recent_turns: vec![(ChatRole::User, "hello".to_string())],
        known_person_names: vec!["Mom".to_string()],
        known_cameras: Vec::new(),
        known_lenses: Vec::new(),
        library: Default::default(),
        current_date_utc: fixed_date(),
    };
    let t0_plan = pipeline.chat.route_turn(ChatTier::Tier0, &input).unwrap();
    assert_eq!(t0_plan.semantic_query.as_deref(), Some("sunset beach"));
    assert!(!pipeline.chat.is_loaded());

    // Tier 2 marks simulated model loaded.
    let _ = pipeline.chat.route_turn(ChatTier::Tier2, &input).unwrap();
    assert!(pipeline.chat.is_loaded());

    // Tier 2 does NOT rerank thumbnails.
    let candidates = vec![
        RerankCandidate {
            asset_id: "a1".to_string(),
            score: 0.90,
            thumb_bytes: vec![1, 2, 3],
            caption_hint: Some("indoor office desk".to_string()),
        },
        RerankCandidate {
            asset_id: "a2".to_string(),
            score: 0.70,
            thumb_bytes: vec![4, 5, 6],
            caption_hint: Some("golden sunset over sandy beach".to_string()),
        },
        RerankCandidate {
            asset_id: "a3".to_string(),
            score: 0.80,
            thumb_bytes: vec![7, 8, 9],
            caption_hint: Some("sunset cliff".to_string()),
        },
    ];
    assert!(pipeline
        .chat
        .rerank_thumbnails(ChatTier::Tier2, "sunset beach", &candidates)
        .unwrap()
        .is_none());

    // Tier 3 reranks candidates deterministically and returns grounded observation.
    let rerank = pipeline
        .chat
        .rerank_thumbnails(ChatTier::Tier3, "sunset beach", &candidates)
        .unwrap()
        .expect("Tier 3 rerank must return RerankResult");
    assert_eq!(rerank.ordered_asset_ids.len(), 3);
    assert_eq!(rerank.ordered_asset_ids[0].0, "a2");
    assert_eq!(rerank.ordered_asset_ids[1].0, "a3");
    assert_eq!(rerank.ordered_asset_ids[2].0, "a1");
    assert!(rerank
        .observation
        .as_deref()
        .is_some_and(|o| o.contains("reranked")));

    // Idle eviction respects max_idle threshold.
    assert_eq!(pipeline.chat.evict_idle(Duration::from_secs(300)), 0);
    assert!(pipeline.chat.is_loaded());

    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(pipeline.chat.evict_idle(Duration::from_millis(10)), 1);
    assert!(!pipeline.chat.is_loaded());
    assert_eq!(pipeline.chat.evict_idle(Duration::from_millis(10)), 0);

    // Direct DeterministicTestChatRouter sanity check.
    let direct = DeterministicTestChatRouter::new();
    assert!(!direct.is_loaded());
}

#[test]
fn sidecar_router_graceful_fallback_without_binary() {
    let dir = tempdir().unwrap();
    let pipeline = AiPipeline::production(dir.path(), dir.path());
    assert!(!pipeline.chat.is_loaded());

    let input = turn_input("videos from Tokyo", RouterPlan::default(), &[]);
    // Even if Tier 2 or Tier 3 is requested without GGUF/llama-server installed, it falls back cleanly.
    let plan = pipeline.chat.route_turn(ChatTier::Tier2, &input).unwrap();
    assert_eq!(plan.kind, Some(AssetKind::Video));
    assert_eq!(plan.location_query.as_deref(), Some("Tokyo"));
    assert!(!pipeline.chat.is_loaded());

    let sidecar = SidecarLlamaRouter::new(dir.path());
    assert!(sidecar
        .rerank_thumbnails(
            ChatTier::Tier3,
            "sunset",
            &[RerankCandidate {
                asset_id: "x".to_string(),
                score: 0.5,
                thumb_bytes: vec![1, 2],
                caption_hint: None,
            }]
        )
        .unwrap()
        .is_none());
    assert_eq!(sidecar.evict_idle(Duration::ZERO), 0);
}

#[test]
fn phase3_manifest_entries_are_apache2_and_exact_filenames() {
    assert_eq!(MODEL_MANIFEST.len(), 12);

    let gemma_e2b = find_model_spec("gemma4-e2b").expect("gemma4-e2b missing");
    assert_eq!(gemma_e2b.license, "Apache-2.0");
    assert_eq!(gemma_e2b.hf_repo, "ggml-org/gemma-4-E2B-it-GGUF");
    assert_eq!(gemma_e2b.tier, Some(truongphoto_core::AiTier::Tier1));
    assert_eq!(gemma_e2b.files.len(), 1);
    assert_eq!(gemma_e2b.files[0].filename, "gemma-4-E2B-it-Q4_0.gguf");

    let gemma_e4b = find_model_spec("gemma4-e4b").expect("gemma4-e4b missing");
    assert_eq!(gemma_e4b.license, "Apache-2.0");
    assert_eq!(gemma_e4b.hf_repo, "ggml-org/gemma-4-E4B-it-GGUF");
    assert_eq!(gemma_e4b.tier, Some(truongphoto_core::AiTier::Tier2));
    assert_eq!(gemma_e4b.files.len(), 1);
    assert_eq!(gemma_e4b.files[0].filename, "gemma-4-E4B-it-Q4_0.gguf");

    let qwen = find_model_spec("qwen3.5-4b").expect("qwen3.5-4b missing");
    assert_eq!(qwen.license, "Apache-2.0");
    assert_eq!(qwen.hf_repo, "ggml-org/Qwen3.5-4B-GGUF");
    assert_eq!(qwen.tier, Some(truongphoto_core::AiTier::Tier2));
    assert_eq!(qwen.files.len(), 1);
    assert_eq!(qwen.files[0].filename, "Qwen3.5-4B-Q4_K_M.gguf");

    let gemma_26b = find_model_spec("gemma4-26b-a4b").expect("gemma4-26b-a4b missing");
    assert_eq!(gemma_26b.license, "Apache-2.0");
    assert_eq!(gemma_26b.hf_repo, "ggml-org/gemma-4-26B-A4B-it-GGUF");
    assert_eq!(gemma_26b.tier, Some(truongphoto_core::AiTier::Tier3));
    assert_eq!(gemma_26b.files.len(), 2);
    assert_eq!(gemma_26b.files[0].filename, "gemma-4-26B-A4B-it-Q4_0.gguf");
    assert_eq!(
        gemma_26b.files[1].filename,
        "mmproj-gemma-4-26B-A4B-it-Q8_0.gguf"
    );

    let basemap = find_model_spec("world-basemap-pmtiles").expect("world-basemap-pmtiles missing");
    assert_eq!(basemap.hf_repo, "Protomaps / OpenStreetMap");
    assert!(basemap.license.contains("ODbL") && basemap.license.contains("BSD"));
    assert_eq!(basemap.tier, None);
    assert_eq!(basemap.files.len(), 1);
    assert_eq!(basemap.files[0].filename, "world-z0-z6.pmtiles");
}

#[test]
fn router_plan_intent_answer_and_merge_isolation() {
    let raw = r#"{
        "intent": "answer",
        "semantic_query": null,
        "ocr_terms": null,
        "person_names": [],
        "location_query": null,
        "camera_model": null,
        "lens_model": null,
        "focal_length_min_mm": null,
        "focal_length_max_mm": null,
        "aperture_min_f": null,
        "aperture_max_f": null,
        "iso_min": null,
        "iso_max": null,
        "shutter_speed_min_sec": null,
        "shutter_speed_max_sec": null,
        "orientation": null,
        "has_people": null,
        "sort_by": null,
        "tags": [],
        "kind": null,
        "favorite": null,
        "date_from": null,
        "date_to": null,
        "clear_prior_filters": false,
        "assistant_reply": "I recognize Sebastian and AJ in your photos.",
        "follow_up_suggestions": ["Show me photos of Sebastian"]
    }"#;
    let parsed = parse_and_validate_router_plan(raw).expect("valid answer intent plan");
    assert_eq!(parsed.intent.as_deref(), Some("answer"));

    // Merging an answer turn over an active search plan must NOT leak prior search filters.
    let prior = RouterPlan {
        intent: Some("search".to_string()),
        location_query: Some("Portland Zoo".to_string()),
        person_names: vec!["Sebastian".to_string()],
        ..Default::default()
    };
    let merged = merge_router_plan(&prior, &parsed);
    assert_eq!(merged.intent.as_deref(), Some("answer"));
    assert_eq!(merged.location_query, None);
    assert!(merged.person_names.is_empty());
    assert_eq!(
        merged.assistant_reply,
        "I recognize Sebastian and AJ in your photos."
    );
}
