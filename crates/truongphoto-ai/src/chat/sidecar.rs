use crate::chat::schema::{
    merge_router_plan, parse_and_validate_router_plan, ROUTER_PLAN_JSON_SCHEMA,
};
use crate::chat::{
    ChatRouterBackend, ChatTurnInput, ClassicChatRouter, RerankCandidate, RerankResult,
};
use crate::models::{find_model_spec, model_dir};
use crate::AiError;
use chrono::{Datelike, NaiveDate};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use truongphoto_core::{ChatRole, ChatTier, RouterPlan};

struct SidecarProcess {
    tier: ChatTier,
    port: u16,
    child: Child,
    last_used: Instant,
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Zero-`cmake` Tier 1–3 conversational router & VLM reranker backed by a managed local
/// `llama-server` HTTP sidecar process, falling back cleanly to [`ClassicChatRouter`]
/// on Tier 0 or when the sidecar binary / GGUF model files are not yet installed.
pub struct SidecarLlamaRouter {
    models_dir: PathBuf,
    classic: ClassicChatRouter,
    process: Mutex<Option<SidecarProcess>>,
}

impl std::fmt::Debug for SidecarLlamaRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarLlamaRouter")
            .field("models_dir", &self.models_dir)
            .field("is_loaded", &self.is_loaded())
            .finish()
    }
}

impl SidecarLlamaRouter {
    pub fn new(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_dir: models_dir.into(),
            classic: ClassicChatRouter::new(),
            process: Mutex::new(None),
        }
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    fn resolve_model_paths(&self, tier: ChatTier) -> Option<(PathBuf, Option<PathBuf>)> {
        let model_id = tier.chat_model_id()?;
        let spec = find_model_spec(model_id)?;
        let dir = model_dir(&self.models_dir, model_id);
        let gguf = dir.join(spec.files.first()?.filename);
        if !gguf.is_file() {
            return None;
        }
        let mmproj = spec
            .files
            .get(1)
            .map(|f| dir.join(f.filename))
            .filter(|p| p.is_file());
        Some((gguf, mmproj))
    }

    fn ensure_server_port(&self, tier: ChatTier) -> Result<Option<u16>, AiError> {
        if tier == ChatTier::Tier0 {
            return Ok(None);
        }
        let Some(bin_path) = discover_llama_server_bin_with_models_dir(&self.models_dir) else {
            return Ok(None);
        };
        let Some((gguf_path, mmproj_path)) = self.resolve_model_paths(tier) else {
            return Ok(None);
        };
        if tier == ChatTier::Tier3 && mmproj_path.is_none() {
            tracing::debug!(
                tier = %tier,
                "Tier 3 mmproj missing; sidecar running text-only unless mmproj is present"
            );
        }

        let mut guard = self.process.lock();
        if let Some(existing) = guard.as_mut() {
            let alive = matches!(existing.child.try_wait(), Ok(None));
            if alive && existing.tier == tier {
                existing.last_used = Instant::now();
                return Ok(Some(existing.port));
            }
            // Tier changed or child exited; drop and respawn cleanly.
            *guard = None;
        }

        let port = pick_ephemeral_port()?;
        let mut cmd = Command::new(&bin_path);
        cmd.arg("-m")
            .arg(&gguf_path)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--reasoning-budget")
            .arg("0")
            .arg("-c")
            .arg("4096")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(bin_dir) = bin_path.parent() {
            cmd.current_dir(bin_dir);
        }
        if let Some(mmproj) = &mmproj_path {
            cmd.arg("--mmproj").arg(mmproj);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| AiError::ExternalTool(format!("spawn {}: {e}", bin_path.display())))?;

        if let Err(e) = wait_for_server_health(port, &mut child, Duration::from_secs(30)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }

        *guard = Some(SidecarProcess {
            tier,
            port,
            child,
            last_used: Instant::now(),
        });
        Ok(Some(port))
    }

    fn touch_last_used(&self) {
        if let Some(proc) = self.process.lock().as_mut() {
            proc.last_used = Instant::now();
        }
    }
}

impl ChatRouterBackend for SidecarLlamaRouter {
    fn route_turn(&self, tier: ChatTier, input: &ChatTurnInput) -> Result<RouterPlan, AiError> {
        let port = match self.ensure_server_port(tier) {
            Ok(Some(port)) => port,
            Ok(None) => return self.classic.route_turn(tier, input),
            Err(err) => {
                tracing::warn!(tier = %tier, error = %err, "llama-server startup failed; falling back to ClassicChatRouter");
                return self.classic.route_turn(tier, input);
            }
        };

        let schema_val: Value = serde_json::from_str(ROUTER_PLAN_JSON_SCHEMA)
            .map_err(|e| AiError::InvalidInput(format!("invalid ROUTER_PLAN_JSON_SCHEMA: {e}")))?;

        let system_prompt = build_router_system_prompt(input);
        let mut messages = vec![json!({
            "role": "system",
            "content": system_prompt,
        })];

        for (role, content) in input.recent_turns.iter().rev().take(6).rev() {
            let role_str = match role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            };
            messages.push(json!({
                "role": role_str,
                "content": content,
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": input.user_message,
        }));

        let payload = json!({
            "model": tier.chat_model_id().unwrap_or("gemma4"),
            "temperature": 0.0,
            "messages": messages,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "RouterPlan",
                    "strict": true,
                    "schema": schema_val,
                }
            }
        });

        let content = match post_chat_completion(port, &payload) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(tier = %tier, error = %err, "llama-server completion failed; falling back to ClassicChatRouter");
                return self.classic.route_turn(tier, input);
            }
        };
        self.touch_last_used();

        match parse_and_validate_router_plan(&content) {
            Ok(mut parsed) => {
                sanitize_sidecar_plan(&input.user_message, &mut parsed);
                Ok(merge_router_plan(&input.prior_filters, &parsed))
            }
            Err(err) => {
                tracing::warn!(tier = %tier, error = %err, "llama-server returned invalid RouterPlan JSON; falling back to ClassicChatRouter");
                self.classic.route_turn(tier, input)
            }
        }
    }

    fn rerank_thumbnails(
        &self,
        tier: ChatTier,
        user_message: &str,
        candidates: &[RerankCandidate],
    ) -> Result<Option<RerankResult>, AiError> {
        if tier != ChatTier::Tier3 || candidates.is_empty() {
            return Ok(None);
        }
        let Some((_, mmproj)) = self.resolve_model_paths(tier) else {
            return Ok(None);
        };
        if mmproj.is_none() {
            return Ok(None);
        }
        let Some(port) = self.ensure_server_port(tier)? else {
            return Ok(None);
        };

        let mut user_parts = vec![json!({
            "type": "text",
            "text": format!(
                "Score each candidate photo/video frame (0.0 to 1.0) for relevance to: \"{}\". Candidate IDs in order: {}",
                user_message.trim(),
                candidates
                    .iter()
                    .map(|c| c.asset_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })];

        for c in candidates {
            if c.thumb_bytes.is_empty() {
                continue;
            }
            let encoded = encode_base64(&c.thumb_bytes);
            user_parts.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/webp;base64,{encoded}")
                }
            }));
        }

        let rerank_schema = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["scores", "observation"],
            "properties": {
                "scores": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["asset_id", "score"],
                        "properties": {
                            "asset_id": { "type": "string" },
                            "score": { "type": "number" }
                        }
                    }
                },
                "observation": { "type": ["string", "null"] }
            }
        });

        let payload = json!({
            "model": tier.chat_model_id().unwrap_or("gemma4-26b-a4b"),
            "temperature": 0.0,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a visual reranker for a private photo library. Score the provided thumbnails strictly against the user query and return valid JSON."
                },
                {
                    "role": "user",
                    "content": user_parts
                }
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "RerankResult",
                    "strict": true,
                    "schema": rerank_schema,
                }
            }
        });

        let content = post_chat_completion(port, &payload)?;
        self.touch_last_used();

        let parsed: SidecarRerankPayload = serde_json::from_str(content.trim()).map_err(|e| {
            AiError::InvalidInput(format!("invalid sidecar VLM rerank response JSON: {e}"))
        })?;

        let score_map: HashMap<String, f32> = parsed
            .scores
            .into_iter()
            .map(|entry| (entry.asset_id, entry.score))
            .collect();

        let mut ordered: Vec<(String, f32)> = candidates
            .iter()
            .map(|c| {
                let s = score_map.get(&c.asset_id).copied().unwrap_or(c.score);
                (c.asset_id.clone(), s)
            })
            .collect();
        ordered.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        Ok(Some(RerankResult {
            ordered_asset_ids: ordered,
            observation: parsed
                .observation
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }))
    }

    fn evict_idle(&self, max_idle: Duration) -> usize {
        let mut guard = self.process.lock();
        let Some(proc) = guard.as_ref() else {
            return 0;
        };
        if Instant::now().saturating_duration_since(proc.last_used) >= max_idle {
            *guard = None;
            1
        } else {
            0
        }
    }

    fn is_loaded(&self) -> bool {
        self.process.lock().is_some()
    }

    fn warmup(&self, tier: ChatTier) -> bool {
        matches!(self.ensure_server_port(tier), Ok(Some(_)))
    }
}

#[derive(Debug, Deserialize)]
struct SidecarScoreEntry {
    asset_id: String,
    score: f32,
}

#[derive(Debug, Deserialize)]
struct SidecarRerankPayload {
    scores: Vec<SidecarScoreEntry>,
    observation: Option<String>,
}

/// Discovers a `llama-server` executable via `TP_LLAMA_SERVER_BIN`, bundled app resource paths,
/// `<data_dir>/runtime/llama`, or standard Homebrew paths.
pub fn discover_llama_server_bin() -> Option<PathBuf> {
    discover_llama_server_bin_opt(None)
}

fn discover_llama_server_bin_with_models_dir(models_dir: &Path) -> Option<PathBuf> {
    discover_llama_server_bin_opt(Some(models_dir))
}

fn discover_llama_server_bin_opt(models_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(val) = std::env::var("TP_LLAMA_SERVER_BIN") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    let bin_name = if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [dir.join(bin_name), dir.join("llama").join(bin_name)] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
            if cfg!(target_os = "macos") {
                if let Some(contents) = dir.parent() {
                    for cand in [
                        contents.join("Resources").join("llama").join(bin_name),
                        contents.join("Resources").join(bin_name),
                    ] {
                        if cand.is_file() {
                            return Some(cand);
                        }
                    }
                }
            }
        }
    }

    if let Some(mdir) = models_dir {
        if let Some(data_dir) = mdir.parent() {
            for cand in [
                data_dir.join("runtime").join("llama").join(bin_name),
                data_dir.join("runtime").join(bin_name),
            ] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }

    if cfg!(target_os = "macos") {
        for cand in [
            "/opt/homebrew/bin/llama-server",
            "/usr/local/bin/llama-server",
        ] {
            let p = Path::new(cand);
            if p.is_file() {
                return Some(p.to_path_buf());
            }
        }
    }

    None
}

fn pick_ephemeral_port() -> Result<u16, AiError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

fn calendar_month_range(year: i32, month: u32) -> (String, String) {
    let (next_y, next_m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let last_day = NaiveDate::from_ymd_opt(next_y, next_m, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28);
    (
        format!("{year:04}-{month:02}-01"),
        format!("{year:04}-{month:02}-{last_day:02}"),
    )
}

fn shift_calendar_month(year: i32, month: u32, delta_months: i32) -> (i32, u32) {
    let total = year * 12 + (month as i32 - 1) + delta_months;
    let y = total.div_euclid(12);
    let m = (total.rem_euclid(12) + 1) as u32;
    (y, m)
}

fn build_router_system_prompt(input: &ChatTurnInput) -> String {
    let prior_json = serde_json::to_string(&input.prior_filters).unwrap_or_else(|_| "{}".into());
    let cur_y = input.current_date_utc.year();
    let cur_m = input.current_date_utc.month();
    let (this_m_from, this_m_to) = calendar_month_range(cur_y, cur_m);
    let (prev1_y, prev1_m) = shift_calendar_month(cur_y, cur_m, -1);
    let (last_m_from, last_m_to) = calendar_month_range(prev1_y, prev1_m);
    let (prev2_y, prev2_m) = shift_calendar_month(cur_y, cur_m, -2);
    let (two_m_from, two_m_to) = calendar_month_range(prev2_y, prev2_m);

    format!(
        "You are TruongPhoto's private offline photo library assistant and query router. Convert the user's message into a strict RouterPlan JSON object.\n\
         Library Context:\n\
         - Current UTC date: {} (today is {})\n\
         - Calendar month reference: \"this month\" = {} to {}; \"last month\" = {} to {}; \"two months ago\" = {} to {}\n\
         - Known RBAC-visible person names: {:?}\n\
         - Known camera bodies in library: {:?}\n\
         - Known lenses in library: {:?}\n\
         - Prior active filters JSON: {}\n\
         Rules:\n\
         - Informational questions about the library itself (e.g. \"what cameras or lenses do I have?\", \"who are the people you know in my photos?\", \"hi\", \"what can you do?\"): answer directly and helpfully in \"assistant_reply\" using Library Context above, set clear_prior_filters=true, and leave ALL filter fields null / [] so no search runs!\n\
         - Put ONLY actual visual subjects, animals, objects, scenes, or moods into \"semantic_query\" (e.g. \"waterfall in forest\", \"ape or monkey behind a fence\", \"mountain\", \"Eiffel Tower\", \"mountain sunrise\"). Generic terrain/nature words like \"mountain\", \"beach\", \"forest\", \"lake\", \"waterfall\", \"sunset\" belong in \"semantic_query\", NOT \"location_query\"! Do NOT put technical EXIF adjectives like \"low light\", \"high ISO\", \"crisp\", \"sharp\", \"wide open\", or \"telephoto\" into \"semantic_query\" when filtering by EXIF numbers.\n\
         - Reserve \"location_query\" strictly for proper named places, cities, countries, parks, zoos, or specific landmarks (e.g. \"Multnomah Falls\", \"Portland Zoo\", \"Mount Saint Helens\", \"Bald Mountain\", \"Tokyo\", \"Antarctica\"). When filtering by a named place, leave \"semantic_query\": null UNLESS they also asked for a specific visual subject there (e.g. \"monkeys at Portland Zoo\" -> location_query: \"Portland Zoo\", semantic_query: \"monkey\").\n\
         - Put specific words/phrases that literally appear inside an image (e.g. \"receipt saying Target\", \"sign with Oregon\") into \"ocr_terms\". If the user asks generally for \"photos with signs or readable text\" without specifying a word, put \"sign or banner with readable text\" into \"semantic_query\" and leave \"ocr_terms\": null.\n\
         - Match people (including typos/first names) against Known RBAC-visible person names into \"person_names\".\n\
         - Camera, Orientation & EXIF fields (NEVER invent EXIF or orientation filters unless the user explicitly mentioned them; NEVER put focal length/lens/aperture/ISO/shutter into \"camera_model\"; framing words like \"close up\" are NOT apertures):\n\
           * \"camera_model\": camera body only (match Known camera bodies, e.g. \"Canon R7\" / \"cannon r7\" -> \"Canon EOS R7\").\n\
           * \"lens_model\": lens name only (match Known lenses, e.g. \"17-40 Art\" -> \"17-40mm F1.8 DC | Art 025\", \"100-400\" -> \"RF100-400mm F5.6-8 IS USM\").\n\
           * \"focal_length_min_mm\" / \"focal_length_max_mm\": numeric mm range. For an EXACT focal length like \"at 12mm\" or \"56mm shots\", you MUST set BOTH min AND max to that exact number (e.g. \"12mm\" -> min: 12, max: 12). Only leave one side null for explicit bounds (\"under 15mm\" -> max: 15; \"over 300mm\" -> min: 300).\n\
           * \"aperture_min_f\" / \"aperture_max_f\": numeric f-stop range. For an EXACT f-stop like \"f/1.4\" or \"narrow aperture f/8\", set BOTH min AND max to that exact number (e.g. \"f/8\" -> min: 8.0, max: 8.0). For \"wide open / bokeh / shallow depth of field\" -> max: 2.0; for \"f/8 or narrower\" -> min: 8.0.\n\
           * \"iso_min\" / \"iso_max\": integer ISO range (exact \"ISO 100\" -> min: 100, max: 100; \"above 3200 / ISO 3200+\" -> min: 3200).\n\
           * \"shutter_speed_min_sec\" / \"shutter_speed_max_sec\": decimal seconds! Faster shutter = SMALLER number (e.g. \"1/1000s or faster\" -> shutter_speed_max_sec: 0.001; \"1/500s or faster\" -> max: 0.002). Slower / long exposure = LARGER number (e.g. \"long exposure / slow shutter / silky water\" -> shutter_speed_min_sec: 0.05; \"1 second or longer\" -> min: 1.0).\n\
           * \"orientation\": \"portrait\" (for vertical / tall / phone wallpaper), \"landscape\" (for horizontal / wide / desktop wallpaper), \"square\", or null.\n\
         - For dates, months, or years, output ISO strings in \"date_from\" and \"date_to\" (format \"YYYY-MM-DD\"). Use full calendar months for \"this month\", \"last month\", and \"two months ago\" matching the Calendar month reference above! Leave both null if no date was mentioned.\n\
         - Leave \"tags\": [] UNLESS the user explicitly asks for a tag (e.g. \"tagged X\" or \"#X\"). Never invent speculative tags.\n\
         - Leave \"kind\": null unless the user explicitly asks for only photos, only videos, raw files, or live photos.\n\
         - Set clear_prior_filters=true when the user starts over, switches topic, or asks to remove a prior filter (re-emit the remaining filters you want to keep); set false when narrowing the previous search.\n\
         - Write a crisp, natural 1-sentence \"assistant_reply\" in active voice (e.g. \"Here are your 12mm photos.\"). Avoid repeating boilerplate.\n\
         - Provide 2-3 short, varied \"follow_up_suggestions\". Never invent file paths or extra JSON keys.",
        input.current_date_utc.to_rfc3339(),
        input.current_date_utc.format("%Y-%m-%d"),
        this_m_from,
        this_m_to,
        last_m_from,
        last_m_to,
        two_m_from,
        two_m_to,
        input.known_person_names,
        input.known_cameras,
        input.known_lenses,
        prior_json,
    )
}

/// Deterministic post-parse safety net for small local LLMs:
/// 1. Strips hallucinated EXIF/orientation bounds when the user never asked about them
///    (e.g., preventing "close up of a monkey" from hallucinating `aperture_max_f: 2.0`).
/// 2. Completes one-sided exact EXIF bounds when the user asked for an exact value like "at 12mm"
///    or "f/8" or "ISO 100" without explicit range/bound phrasing ("over", "under", "wider", etc.).
fn sanitize_sidecar_plan(user_message: &str, plan: &mut RouterPlan) {
    let lower = user_message.to_ascii_lowercase();

    let has_lower_bound_cue = [
        "over ",
        "above ",
        "more than",
        "at least",
        "longer than",
        "greater than",
        "higher than",
        "narrower than",
        "or narrower",
        "or slower",
        "or longer",
        "zoomed",
        "beyond ",
        ">=",
        ">",
        "+",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    let has_upper_bound_cue = [
        "under ",
        "below ",
        "less than",
        "shorter than",
        "wider than",
        "or wider",
        "or faster",
        "lower than",
        "at most",
        "up to ",
        "<=",
        "<",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    let has_aperture_cue = lower.contains("f/")
        || lower.contains("aperture")
        || lower.contains("wide open")
        || lower.contains("wide-open")
        || lower.contains("bokeh")
        || lower.contains("shallow")
        || lower.contains("f-stop")
        || lower.contains("f stop")
        || lower.contains("depth of field")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .any(|tok| {
                tok.starts_with('f')
                    && tok.len() >= 2
                    && tok[1..].chars().all(|c| c.is_ascii_digit() || c == '.')
                    && tok[1..].chars().any(|c| c.is_ascii_digit())
            });

    if !has_aperture_cue {
        plan.aperture_min_f = None;
        plan.aperture_max_f = None;
    } else if let (Some(lo), None) = (plan.aperture_min_f, plan.aperture_max_f) {
        if !has_lower_bound_cue {
            plan.aperture_max_f = Some(lo);
        }
    } else if let (None, Some(hi)) = (plan.aperture_min_f, plan.aperture_max_f) {
        if !has_upper_bound_cue
            && !lower.contains("wide open")
            && !lower.contains("wide-open")
            && !lower.contains("bokeh")
            && !lower.contains("shallow")
        {
            plan.aperture_min_f = Some(hi);
        }
    }

    let has_iso_cue = lower.contains("iso")
        || lower.contains("grain")
        || lower.contains("noisy")
        || lower.contains("noise");

    if !has_iso_cue {
        plan.iso_min = None;
        plan.iso_max = None;
    } else if let (Some(lo), None) = (plan.iso_min, plan.iso_max) {
        if !has_lower_bound_cue && !lower.contains("high iso") {
            plan.iso_max = Some(lo);
        }
    } else if let (None, Some(hi)) = (plan.iso_min, plan.iso_max) {
        if !has_upper_bound_cue && !lower.contains("low iso") {
            plan.iso_min = Some(hi);
        }
    }

    let has_focal_cue = lower.contains("mm")
        || lower.contains("focal")
        || lower.contains("wide")
        || lower.contains("telephoto")
        || lower.contains("zoom")
        || lower.contains("fisheye");

    if !has_focal_cue {
        plan.focal_length_min_mm = None;
        plan.focal_length_max_mm = None;
    } else if let (Some(lo), None) = (plan.focal_length_min_mm, plan.focal_length_max_mm) {
        if !has_lower_bound_cue && !lower.contains("telephoto") {
            plan.focal_length_max_mm = Some(lo);
        }
    } else if let (None, Some(hi)) = (plan.focal_length_min_mm, plan.focal_length_max_mm) {
        if !has_upper_bound_cue && !lower.contains("wide") && !lower.contains("fisheye") {
            plan.focal_length_min_mm = Some(hi);
        }
    }

    let has_shutter_cue = lower.contains("shutter")
        || lower.contains("exposure")
        || lower.contains("silky")
        || lower.contains("motion blur")
        || lower.contains("freeze")
        || lower.contains("1/")
        || lower.contains("sec");

    if !has_shutter_cue {
        plan.shutter_speed_min_sec = None;
        plan.shutter_speed_max_sec = None;
    } else if let (None, Some(v)) = (plan.shutter_speed_min_sec, plan.shutter_speed_max_sec) {
        if lower.contains("long exposure")
            || lower.contains("slow shutter")
            || lower.contains("silky")
            || lower.contains("or slower")
            || lower.contains("or longer")
        {
            plan.shutter_speed_min_sec = Some(v);
            plan.shutter_speed_max_sec = None;
        }
    } else if let (Some(v), None) = (plan.shutter_speed_min_sec, plan.shutter_speed_max_sec) {
        if lower.contains("fast shutter") || lower.contains("or faster") || lower.contains("freeze")
        {
            plan.shutter_speed_max_sec = Some(v);
            plan.shutter_speed_min_sec = None;
        }
    }

    let is_who_are_people_info_q = (lower.contains("who are the people")
        || lower.contains("what people")
        || lower.contains("which people")
        || lower.contains("list the people")
        || lower.contains("who do you know"))
        && !lower.contains("show me")
        && !lower.contains("find ");
    if is_who_are_people_info_q && plan.person_names.len() >= 3 {
        plan.person_names.clear();
    }

    let has_orientation_cue = lower.contains("vertical")
        || lower.contains("portrait")
        || lower.contains("tall")
        || lower.contains("wallpaper")
        || lower.contains("horizontal")
        || lower.contains("landscape")
        || lower.contains("square");

    if !has_orientation_cue {
        plan.orientation = None;
    }
}

fn run_blocking_http<F, T>(fut_fn: F) -> Result<T, AiError>
where
    F: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, AiError>> + Send>>
        + Send,
    T: Send,
{
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| AiError::ExternalTool(format!("tokio runtime: {e}")))?;
            rt.block_on(fut_fn())
        })
        .join()
        .map_err(|_| AiError::ExternalTool("sidecar HTTP thread panicked".to_string()))?
    })
}

fn wait_for_server_health(port: u16, child: &mut Child, timeout: Duration) -> Result<(), AiError> {
    let deadline = Instant::now() + timeout;
    let url = format!("http://127.0.0.1:{port}/health");
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| AiError::ExternalTool(format!("llama-server wait: {e}")))?
        {
            return Err(AiError::ExternalTool(format!(
                "llama-server exited early with status {status}"
            )));
        }

        let ok = run_blocking_http(|| {
            let url = url.clone();
            Box::pin(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_millis(500))
                    .build()
                    .map_err(|e| AiError::ExternalTool(e.to_string()))?;
                Ok(client
                    .get(&url)
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success()))
            })
        })?;
        if ok {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(AiError::ExternalTool(
                "timed out waiting for llama-server /health".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn post_chat_completion(port: u16, payload: &Value) -> Result<String, AiError> {
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = payload.clone();

    run_blocking_http(|| {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|e| AiError::ExternalTool(e.to_string()))?;

            let resp = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| AiError::ExternalTool(format!("POST {url}: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(AiError::ExternalTool(format!(
                    "llama-server returned HTTP {status}: {text}"
                )));
            }

            let val: Value = resp
                .json()
                .await
                .map_err(|e| AiError::ExternalTool(format!("decode llama-server JSON: {e}")))?;

            let content = val
                .pointer("/choices/0/message/content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    AiError::ExternalTool(
                        "llama-server response missing choices[0].message.content".to_string(),
                    )
                })?;

            Ok(content.to_string())
        })
    })
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
