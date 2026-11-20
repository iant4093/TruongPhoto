pub mod admin;
pub mod ai_admin;
pub mod albums;
pub mod auth;
pub mod chat;
pub mod folders;
pub mod health;
pub mod library;
pub mod map;
pub mod media;
pub mod people;
pub mod search;
pub mod setup;
pub mod tailscale;
pub mod updates;
pub mod uploads;

use crate::assets_embed::api_not_found;
use crate::state::AppState;
use crate::ws::ws_handler;
use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, patch, post, put},
    Router,
};
use std::sync::Arc;

/// Builds the `/api/*` router with exact Axum 0.8 `{id}` path syntax and JSON 404 fallback.
pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        // Health & system
        .route("/health", get(health::health))
        .route("/system/info", get(health::system_info))
        .route("/system/qr", get(tailscale::system_qr))
        // First-run setup
        .route("/setup/status", get(setup::setup_status))
        .route("/setup/initialize", post(setup::setup_initialize))
        // Auth & sessions
        .route("/auth/status", get(auth::auth_status))
        .route("/auth/server-pin", post(auth::server_pin))
        .route("/auth/login", post(auth::login))
        .route("/auth/desktop-bootstrap", post(auth::desktop_bootstrap))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        // Watched folders & scans
        .route(
            "/folders",
            get(folders::list_folders).post(folders::create_folder),
        )
        .route(
            "/folders/{id}",
            patch(folders::update_folder).delete(folders::delete_folder),
        )
        .route("/folders/{id}/scan", post(folders::scan_folder))
        .route("/folders/scan-all", post(folders::scan_all_folders))
        // Admin endpoints
        .route(
            "/admin/users",
            get(admin::list_users).post(admin::create_user),
        )
        .route(
            "/admin/users/{id}",
            patch(admin::update_user).delete(admin::delete_user),
        )
        .route("/admin/server-pin", put(admin::update_server_pin))
        .route(
            "/admin/settings",
            get(admin::get_settings).patch(admin::update_settings),
        )
        .route("/admin/backup", post(admin::trigger_backup))
        .route("/admin/backups", get(admin::list_backups))
        .route("/admin/tailscale/status", get(tailscale::tailscale_status))
        .route("/admin/tailscale/enable", post(tailscale::enable_tailscale))
        .route(
            "/admin/tailscale/disable",
            post(tailscale::disable_tailscale),
        )
        .route("/admin/updates/check", get(updates::check_for_updates))
        // Phase 2 Admin AI Models & Indexing
        .route("/admin/ai/models", get(ai_admin::list_ai_models))
        .route(
            "/admin/ai/models/{id}/download",
            post(ai_admin::download_ai_model),
        )
        .route(
            "/admin/ai/models/{id}/cancel",
            post(ai_admin::cancel_ai_model_download),
        )
        .route("/admin/ai/models/{id}", delete(ai_admin::delete_ai_model))
        .route("/admin/ai/tier", post(ai_admin::set_ai_tier))
        .route("/admin/ai/chat-tier", post(ai_admin::set_chat_tier))
        .route("/admin/ai/reindex", post(ai_admin::reindex_ai))
        .route("/admin/ai/pause", post(ai_admin::pause_ai))
        .route("/admin/ai/resume", post(ai_admin::resume_ai))
        .route("/admin/ai/settings", patch(ai_admin::update_ai_settings))
        // Library, search, conversational search, asset details, overrides, tags, trash & shareable users
        .route("/search", post(search::search_library))
        .route(
            "/chat/sessions",
            get(chat::list_sessions).post(chat::create_session),
        )
        .route(
            "/chat/sessions/{id}",
            get(chat::get_session).delete(chat::delete_session),
        )
        .route("/chat/sessions/{id}/messages", post(chat::send_message))
        .route("/chat/warmup", post(chat::warmup_chat))
        // Hybrid offline-first map view
        .route("/map/config", get(map::get_map_config))
        .route("/map/clusters", get(map::list_map_clusters))
        .route("/map/points", get(map::list_map_points))
        .route("/map/tiles/{name}", get(map::get_pmtiles_tile))
        .route("/assets", get(library::list_assets))
        .route("/assets/buckets", get(library::list_asset_buckets))
        .route("/assets/trash", post(library::trash_assets))
        .route("/assets/{id}", get(library::get_asset_detail))
        .route("/assets/{id}/favorite", patch(library::set_favorite))
        .route("/assets/{id}/overrides", patch(library::update_overrides))
        .route("/assets/{id}/tags", post(library::add_tag))
        .route("/assets/{id}/tags/{tag}", delete(library::remove_tag))
        // Phase 2 People & Face Clustering
        .route("/people", get(people::list_people))
        .route("/people/merge", post(people::merge_people))
        .route("/people/cluster", post(people::cluster_people))
        .route(
            "/people/{id}",
            get(people::get_person).patch(people::update_person),
        )
        .route("/people/{id}/assets", get(people::list_person_assets))
        .route(
            "/people/{id}/faces/{face_id}/reassign",
            post(people::reassign_face),
        )
        .route("/trash", get(library::list_trash))
        .route("/trash/restore", post(library::restore_trash))
        .route("/trash/purge", post(library::purge_trash))
        .route("/users/shareable", get(library::list_shareable_users))
        // Media streaming, renditions & face thumbnails
        .route("/media/faces/{face_id}/thumb", get(people::get_face_thumb))
        .route("/media/{id}/thumb", get(media::get_thumb))
        .route("/media/{id}/preview", get(media::get_preview))
        .route("/media/{id}/original", get(media::get_original))
        .route("/media/{id}/live-video", get(media::get_live_video))
        .route("/media/{id}/hls/index.m3u8", get(media::get_hls_playlist))
        .route("/media/{id}/hls/{segment}", get(media::get_hls_segment))
        // Streaming multipart uploads (disables global 4 MiB body limit on this route ONLY)
        .route(
            "/uploads",
            post(uploads::upload_files).layer(DefaultBodyLimit::disable()),
        )
        // Albums CRUD & sharing
        .route(
            "/albums",
            get(albums::list_albums).post(albums::create_album),
        )
        .route(
            "/albums/{id}",
            get(albums::get_album)
                .patch(albums::update_album)
                .delete(albums::delete_album),
        )
        .route(
            "/albums/{id}/assets",
            post(albums::add_album_assets).delete(albums::remove_album_assets),
        )
        .route("/albums/{id}/shares", put(albums::replace_album_shares))
        // Live WebSocket channel
        .route("/ws", get(ws_handler))
        // Unmatched /api/* -> unified JSON 404
        .fallback(api_not_found)
}
