use crate::chat::schema::{
    merge_router_plan, parse_and_validate_router_plan, ROUTER_PLAN_JSON_SCHEMA,
};
use crate::chat::{
    ChatRouterBackend, ChatTurnInput, ClassicChatRouter, RerankCandidate, RerankResult,
};
use crate::models::{find_model_spec, model_dir};
use crate::AiError;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use truongphoto_core::{AssetKind, ChatRole, ChatTier, RouterPlan};

struct SidecarProcess {
    tier: ChatTier,
    port: u16,
    cpu_only: bool,
    has_mmproj: bool,
    child: Child,
    last_used: Instant,
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct InFlightGuard<'a>(&'a AtomicUsize);

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Zero-`cmake` Tier 1–3 conversational router & VLM reranker backed by a managed local
/// `llama-server` HTTP sidecar process, falling back cleanly to CPU offload when GPU VRAM
/// or drivers fail, and to [`ClassicChatRouter`] when no binary/model is installed.
pub struct SidecarLlamaRouter {
    models_dir: PathBuf,
    classic: ClassicChatRouter,
    process: Mutex<Option<SidecarProcess>>,
    /// Lock-free readiness flag so `is_loaded()` never blocks while the server is starting up.
    loaded: AtomicBool,
    /// Tiers latched to CPU-only (`-ngl 0 -fa off`) after a GPU spawn or VRAM OOM failure.
    cpu_fallback_tiers: Mutex<HashSet<ChatTier>>,
    /// Set if an older `llama-server` binary rejects `--reasoning-budget`.
    no_reasoning_budget: AtomicBool,
    in_flight: AtomicUsize,
    shutting_down: AtomicBool,
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
            loaded: AtomicBool::new(false),
            cpu_fallback_tiers: Mutex::new(HashSet::new()),
            no_reasoning_budget: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    fn resolve_model_paths(&self, tier: ChatTier) -> Option<(PathBuf, Option<PathBuf>)> {
        let primary_id = tier.chat_model_id()?;
        // For Tier 2, gracefully fall back to `qwen3.5-4b` if the user downloaded that alternative
        // instead of `gemma4-e4b`.
        let candidate_ids: &[&str] = if tier == ChatTier::Tier2 {
            &[primary_id, "qwen3.5-4b"]
        } else {
            &[primary_id]
        };
        for &model_id in candidate_ids {
            let Some(spec) = find_model_spec(model_id) else {
                continue;
            };
            let dir = model_dir(&self.models_dir, model_id);
            let Some(first_file) = spec.files.first() else {
                continue;
            };
            let gguf = dir.join(first_file.filename);
            if !gguf.is_file() {
                continue;
            }
            let mmproj = spec
                .files
                .get(1)
                .map(|f| dir.join(f.filename))
                .filter(|p| p.is_file());
            return Some((gguf, mmproj));
        }
        None
    }

    fn spawn_and_wait_server(
        &self,
        bin_path: &Path,
        gguf_path: &Path,
        mmproj_path: Option<&Path>,
        tier: ChatTier,
        cpu_only: bool,
        use_reasoning_budget: bool,
    ) -> Result<SidecarProcess, (AiError, String)> {
        let port = pick_ephemeral_port().map_err(|e| (e, String::new()))?;
        // 8192 context for text tiers prevents HTTP 400 overflow on large libraries with rich
        // overviews + multi-turn history, while staying lightweight (~150-250 MB KV cache).
        let ctx_size = if tier == ChatTier::Tier3 && mmproj_path.is_some() {
            "16384"
        } else {
            "8192"
        };
        let mut cmd = Command::new(bin_path);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.arg("-m")
            .arg(gguf_path)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("-c")
            .arg(ctx_size)
            .arg("-np")
            .arg("1")
            .arg("--cache-reuse")
            .arg("256")
            .arg("-fa")
            .arg(if cpu_only { "off" } else { "on" })
            .arg("-ngl")
            .arg(if cpu_only { "0" } else { "99" })
            .arg("--no-webui")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if use_reasoning_budget {
            cmd.arg("--reasoning-budget").arg("0");
        }
        if let Some(bin_dir) = bin_path.parent() {
            cmd.current_dir(bin_dir);
        }
        if let Some(mmproj) = mmproj_path {
            cmd.arg("--mmproj").arg(mmproj);
        }

        let mut child = cmd.spawn().map_err(|e| {
            (
                AiError::ExternalTool(format!("spawn {}: {e}", bin_path.display())),
                String::new(),
            )
        })?;
        truongphoto_core::process::assign_to_kill_on_close_job(&child);

        // Drain stderr continuously into a small bounded ring buffer so the pipe never stalls
        // and startup failures include actionable Vulkan/CUDA/GGUF diagnostics.
        let stderr_tail: std::sync::Arc<Mutex<std::collections::VecDeque<String>>> =
            std::sync::Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(24)));
        if let Some(stderr) = child.stderr.take() {
            let tail_ref = std::sync::Arc::clone(&stderr_tail);
            let _ = std::thread::Builder::new()
                .name("tp-llama-stderr".into())
                .spawn(move || {
                    use std::io::{BufRead, BufReader};
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let mut q = tail_ref.lock();
                        if q.len() >= 24 {
                            q.pop_front();
                        }
                        q.push_back(trimmed.chars().take(240).collect());
                    }
                });
        }

        let health_timeout = if tier == ChatTier::Tier3 {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(45)
        };
        if let Err(e) =
            wait_for_server_health(port, &mut child, health_timeout, &self.shutting_down)
        {
            let _ = child.kill();
            let _ = child.wait();
            let raw_stderr = {
                let q = stderr_tail.lock();
                q.iter().cloned().collect::<Vec<_>>().join(" ; ")
            };
            let tail_msg = if raw_stderr.is_empty() {
                String::new()
            } else {
                format!(" | stderr: {raw_stderr}")
            };
            return Err((AiError::ExternalTool(format!("{e}{tail_msg}")), raw_stderr));
        }

        Ok(SidecarProcess {
            tier,
            port,
            cpu_only,
            has_mmproj: mmproj_path.is_some(),
            child,
            last_used: Instant::now(),
        })
    }

    fn ensure_server_port(&self, tier: ChatTier) -> Result<Option<u16>, AiError> {
        if tier == ChatTier::Tier0 || self.shutting_down.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Some(bin_path) = discover_llama_server_bin_with_models_dir(&self.models_dir) else {
            return Ok(None);
        };
        let Some((gguf_path, mmproj_path)) = self.resolve_model_paths(tier) else {
            return Ok(None);
        };
        let want_mmproj = mmproj_path.is_some();
        if tier == ChatTier::Tier3 && !want_mmproj {
            tracing::debug!(
                tier = %tier,
                "Tier 3 mmproj missing; sidecar running text-only unless mmproj is present"
            );
        }

        let mut guard = self.process.lock();
        if self.shutting_down.load(Ordering::Acquire) {
            return Ok(None);
        }
        let want_cpu = self.cpu_fallback_tiers.lock().contains(&tier);
        if let Some(existing) = guard.as_mut() {
            let alive = matches!(existing.child.try_wait(), Ok(None));
            if alive
                && existing.tier == tier
                && existing.cpu_only == want_cpu
                && existing.has_mmproj == want_mmproj
            {
                existing.last_used = Instant::now();
                return Ok(Some(existing.port));
            }
            // Tier, offload mode, or mmproj availability changed, or child exited; drop and respawn cleanly.
            *guard = None;
            self.loaded.store(false, Ordering::Release);
        }

        let use_rb = !self.no_reasoning_budget.load(Ordering::Acquire);
        let mut last_err: Option<(AiError, String)> = None;
        for attempt in 0..3 {
            match self.spawn_and_wait_server(
                &bin_path,
                &gguf_path,
                mmproj_path.as_deref(),
                tier,
                want_cpu,
                use_rb,
            ) {
                Ok(proc) => {
                    let port = proc.port;
                    *guard = Some(proc);
                    self.loaded.store(true, Ordering::Release);
                    return Ok(Some(port));
                }
                Err((err, stderr_text)) => {
                    let low_err = stderr_text.to_ascii_lowercase();
                    let is_port_conflict = low_err.contains("address already in use")
                        || low_err.contains("failed to listen")
                        || low_err.contains("failed to bind")
                        || low_err.contains("eaddrinuse");
                    if is_port_conflict
                        && attempt < 2
                        && !self.shutting_down.load(Ordering::Acquire)
                    {
                        tracing::warn!(tier = %tier, attempt, "llama-server ephemeral port collision; retrying with fresh port");
                        last_err = Some((err, stderr_text));
                        continue;
                    }
                    last_err = Some((err, stderr_text));
                    break;
                }
            }
        }
        let (err, stderr_text) = last_err.expect("spawn loop always produces result");
        let low_err = stderr_text.to_ascii_lowercase();
        // 1. Retry without `--reasoning-budget` if an older llama-server binary rejects the flag.
        if use_rb
            && (low_err.contains("reasoning-budget")
                || low_err.contains("unknown argument")
                || low_err.contains("invalid argument"))
        {
            tracing::warn!(
                tier = %tier,
                "llama-server rejected --reasoning-budget; latching off and retrying"
            );
            self.no_reasoning_budget.store(true, Ordering::Release);
            if let Ok(proc) = self.spawn_and_wait_server(
                &bin_path,
                &gguf_path,
                mmproj_path.as_deref(),
                tier,
                want_cpu,
                false,
            ) {
                let port = proc.port;
                *guard = Some(proc);
                self.loaded.store(true, Ordering::Release);
                return Ok(Some(port));
            }
        }
        // 2. If GPU offload failed (Windows Vulkan driver missing/old or discrete VRAM OOM),
        // latch CPU-only (`-ngl 0 -fa off`) for this tier and retry once so local LLM still works!
        if !want_cpu && !self.shutting_down.load(Ordering::Acquire) {
            tracing::warn!(
                tier = %tier,
                error = %err,
                "llama-server GPU launch failed; latching CPU fallback (-ngl 0) and retrying"
            );
            self.cpu_fallback_tiers.lock().insert(tier);
            let use_rb_retry = !self.no_reasoning_budget.load(Ordering::Acquire);
            match self.spawn_and_wait_server(
                &bin_path,
                &gguf_path,
                mmproj_path.as_deref(),
                tier,
                true,
                use_rb_retry,
            ) {
                Ok(proc) => {
                    let port = proc.port;
                    *guard = Some(proc);
                    self.loaded.store(true, Ordering::Release);
                    return Ok(Some(port));
                }
                Err((cpu_err, _)) => return Err(cpu_err),
            }
        }
        Err(err)
    }

    fn clear_if_dead(&self) -> bool {
        let mut guard = self.process.lock();
        if let Some(existing) = guard.as_mut() {
            if !matches!(existing.child.try_wait(), Ok(None)) {
                *guard = None;
                self.loaded.store(false, Ordering::Release);
                return true;
            }
        }
        false
    }

    fn latch_cpu_fallback_and_restart(&self, tier: ChatTier) -> bool {
        let mut set = self.cpu_fallback_tiers.lock();
        if !set.insert(tier) {
            return false;
        }
        drop(set);
        let mut guard = self.process.lock();
        *guard = None;
        self.loaded.store(false, Ordering::Release);
        true
    }

    fn touch_last_used(&self) {
        if let Some(proc) = self.process.lock().as_mut() {
            proc.last_used = Instant::now();
        }
    }
}

impl ChatRouterBackend for SidecarLlamaRouter {
    fn route_turn(&self, tier: ChatTier, input: &ChatTurnInput) -> Result<RouterPlan, AiError> {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let _in_flight_guard = InFlightGuard(&self.in_flight);
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

        let system_prompt = build_router_system_prompt(tier, input);
        let mut messages = vec![json!({
            "role": "system",
            "content": system_prompt,
        })];

        for (role, content) in input.recent_turns.iter().rev().take(6).rev() {
            let role_str = match role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            };
            let truncated: String = content.trim().chars().take(240).collect();
            messages.push(json!({
                "role": role_str,
                "content": truncated,
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": input.user_message,
        }));

        let payload = json!({
            "model": tier.chat_model_id().unwrap_or("gemma4"),
            "temperature": 0.0,
            "repeat_penalty": 1.08,
            "max_tokens": 640,
            "cache_prompt": true,
            "id_slot": 0,
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

        let content = match post_chat_completion(port, &payload, Duration::from_secs(60)) {
            Ok(c) => c,
            Err(err) => {
                let died = self.clear_if_dead();
                let err_low = err.to_string().to_ascii_lowercase();
                let is_gpu_or_oom = died
                    || err_low.contains("out of memory")
                    || err_low.contains("oom")
                    || err_low.contains("vulkan")
                    || err_low.contains("metal")
                    || err_low.contains("cuda")
                    || err_low.contains("connection reset")
                    || err_low.contains("broken pipe");
                // Only latch CPU fallback on genuine sidecar crash/GPU/VRAM failure — NEVER on HTTP 400 or read timeout.
                if is_gpu_or_oom && self.latch_cpu_fallback_and_restart(tier) {
                    tracing::warn!(tier = %tier, error = %err, "llama-server inference failed on GPU; retrying on CPU");
                    if let Ok(Some(cpu_port)) = self.ensure_server_port(tier) {
                        if let Ok(c) =
                            post_chat_completion(cpu_port, &payload, Duration::from_secs(90))
                        {
                            self.touch_last_used();
                            if let Ok(mut parsed) = parse_and_validate_router_plan(&c) {
                                sanitize_sidecar_plan(
                                    &input.user_message,
                                    &input.prior_filters,
                                    &input.library.known_folders,
                                    &input.known_person_names,
                                    input.current_date_utc,
                                    &mut parsed,
                                );
                                return Ok(merge_router_plan(&input.prior_filters, &parsed));
                            }
                        }
                    }
                }
                tracing::warn!(tier = %tier, error = %err, "llama-server completion failed; falling back to ClassicChatRouter");
                return self.classic.route_turn(tier, input);
            }
        };
        self.touch_last_used();

        match parse_and_validate_router_plan(&content) {
            Ok(mut parsed) => {
                sanitize_sidecar_plan(
                    &input.user_message,
                    &input.prior_filters,
                    &input.library.known_folders,
                    &input.known_person_names,
                    input.current_date_utc,
                    &mut parsed,
                );
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
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let _in_flight_guard = InFlightGuard(&self.in_flight);
        let Some(port) = self.ensure_server_port(tier)? else {
            return Ok(None);
        };

        let valid_candidates: Vec<&RerankCandidate> = candidates
            .iter()
            .filter(|c| !c.thumb_bytes.is_empty())
            .collect();
        if valid_candidates.is_empty() {
            return Ok(None);
        }

        // Interleave short 1-based integer indices + metadata caption_hints directly before each
        // thumbnail so the VLM binds every image cleanly without ever having to copy 36-char UUIDs.
        let mut user_parts = vec![json!({
            "type": "text",
            "text": format!(
                "User request: \"{}\"\nEvaluate the {} candidate photos/frames below. Score each candidate from 0.0 (irrelevant/poor match) to 1.0 (exact, standout visual match) using their 1-based index (1..={}).",
                user_message.trim(),
                valid_candidates.len(),
                valid_candidates.len()
            )
        })];

        for (idx, c) in valid_candidates.iter().enumerate() {
            let num = idx + 1;
            let label = match c
                .caption_hint
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(hint) => format!("Image #{num} (metadata: {hint}):"),
                None => format!("Image #{num}:"),
            };
            user_parts.push(json!({
                "type": "text",
                "text": label,
            }));

            let mime = if c.thumb_bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                "image/jpeg"
            } else if c.thumb_bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                "image/png"
            } else {
                "image/webp"
            };
            let encoded = encode_base64(&c.thumb_bytes);
            user_parts.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{mime};base64,{encoded}")
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
                        "required": ["index", "score"],
                        "properties": {
                            "index": { "type": "integer", "minimum": 1, "maximum": valid_candidates.len() },
                            "score": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
                        }
                    }
                },
                "observation": { "type": ["string", "null"] }
            }
        });

        let payload = json!({
            "model": tier.chat_model_id().unwrap_or("gemma4-26b-a4b"),
            "temperature": 0.0,
            "repeat_penalty": 1.05,
            "max_tokens": 640,
            "cache_prompt": false,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a visual reranker for a private photo library. Visually inspect every numbered image and score how well it matches the user's request (0.0 = unrelated, 0.5 = partial, 0.9-1.0 = best match). Return valid JSON with integer `index` (1..N) and `score` (0.0..1.0). In `observation`, write one concise, helpful natural sentence describing what stands out visually in the top matches — NEVER mention image numbers, candidate indices, scores, or IDs."
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

        let content = match post_chat_completion(port, &payload, Duration::from_secs(150)) {
            Ok(c) => c,
            Err(e) => {
                self.clear_if_dead();
                return Err(e);
            }
        };
        self.touch_last_used();

        let trimmed = strip_fence_for_rerank(&content);
        let parsed: SidecarRerankPayload = serde_json::from_str(trimmed).map_err(|e| {
            AiError::InvalidInput(format!("invalid sidecar VLM rerank response JSON: {e}"))
        })?;

        let mut score_map: HashMap<String, f32> = HashMap::with_capacity(valid_candidates.len());
        for entry in parsed.scores {
            let clamped = if entry.score.is_finite() {
                entry.score.clamp(0.0, 1.0)
            } else {
                continue;
            };
            if let Some(idx_1based) = entry.index {
                if idx_1based >= 1 && idx_1based <= valid_candidates.len() {
                    let aid = &valid_candidates[idx_1based - 1].asset_id;
                    score_map.insert(aid.clone(), clamped);
                }
            } else if let Some(aid) = entry.asset_id {
                score_map.insert(aid, clamped);
            }
        }

        // Return ONLY candidates that the VLM actually scored so omitted/missing-thumbnail items
        // preserve their hybrid order instead of being falsely demoted as `< 0.15` VLM rejections.
        let mut ordered: Vec<(String, f32)> = candidates
            .iter()
            .filter_map(|c| {
                score_map
                    .get(&c.asset_id)
                    .copied()
                    .map(|s| (c.asset_id.clone(), s))
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
        if !max_idle.is_zero() && self.in_flight.load(Ordering::Acquire) > 0 {
            return 0;
        }
        let mut guard = self.process.lock();
        let Some(proc) = guard.as_ref() else {
            return 0;
        };
        if Instant::now().saturating_duration_since(proc.last_used) >= max_idle {
            *guard = None;
            self.loaded.store(false, Ordering::Release);
            1
        } else {
            0
        }
    }

    fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    fn is_loaded_for_tier(&self, tier: ChatTier) -> bool {
        if tier == ChatTier::Tier0 {
            return false;
        }
        let want_mmproj = self
            .resolve_model_paths(tier)
            .is_some_and(|(_, mmproj)| mmproj.is_some());
        let mut guard = self.process.lock();
        if let Some(existing) = guard.as_mut() {
            if matches!(existing.child.try_wait(), Ok(None)) {
                return existing.tier == tier && existing.has_mmproj == want_mmproj;
            }
            *guard = None;
            self.loaded.store(false, Ordering::Release);
        }
        false
    }

    fn warmup(&self, tier: ChatTier) -> bool {
        matches!(self.ensure_server_port(tier), Ok(Some(_)))
    }

    fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.loaded.store(false, Ordering::Release);
        let mut guard = self.process.lock();
        *guard = None;
    }
}

fn strip_fence_for_rerank(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or(rest);
        body.trim()
            .strip_suffix("```")
            .map(str::trim)
            .unwrap_or_else(|| body.trim())
    } else {
        trimmed
    }
}

#[derive(Debug, Deserialize)]
struct SidecarScoreEntry {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    asset_id: Option<String>,
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
            for cand in [
                dir.join(bin_name),
                dir.join("llama").join(bin_name),
                dir.join("resources").join("llama").join(bin_name),
            ] {
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
    } else if cfg!(target_os = "windows") {
        let mut win_dirs = vec![
            PathBuf::from(r"C:\ProgramData\chocolatey\bin"),
            PathBuf::from(r"C:\llama.cpp\bin"),
        ];
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            win_dirs.push(
                PathBuf::from(local_app)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links"),
            );
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            win_dirs.push(PathBuf::from(profile).join("scoop").join("shims"));
        }
        for dir in win_dirs {
            let cand = dir.join(bin_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(bin_name);
            if cand.is_file() {
                return Some(cand);
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

fn compact_prior_filters_json(prior: &RouterPlan) -> String {
    let mut map = serde_json::Map::new();
    if let Some(v) = &prior.semantic_query {
        map.insert("semantic_query".into(), json!(v));
    }
    if let Some(v) = &prior.ocr_terms {
        map.insert("ocr_terms".into(), json!(v));
    }
    if !prior.person_names.is_empty() {
        map.insert("person_names".into(), json!(prior.person_names));
    }
    if !prior.exclude_person_names.is_empty() {
        map.insert(
            "exclude_person_names".into(),
            json!(prior.exclude_person_names),
        );
    }
    if let Some(v) = &prior.location_query {
        map.insert("location_query".into(), json!(v));
    }
    if let Some(v) = &prior.exclude_location_query {
        map.insert("exclude_location_query".into(), json!(v));
    }
    if let Some(v) = &prior.camera_model {
        map.insert("camera_model".into(), json!(v));
    }
    if let Some(v) = &prior.lens_model {
        map.insert("lens_model".into(), json!(v));
    }
    if let Some(v) = prior.focal_length_min_mm {
        map.insert("focal_length_min_mm".into(), json!(v));
    }
    if let Some(v) = prior.focal_length_max_mm {
        map.insert("focal_length_max_mm".into(), json!(v));
    }
    if let Some(v) = prior.aperture_min_f {
        map.insert("aperture_min_f".into(), json!(v));
    }
    if let Some(v) = prior.aperture_max_f {
        map.insert("aperture_max_f".into(), json!(v));
    }
    if let Some(v) = prior.iso_min {
        map.insert("iso_min".into(), json!(v));
    }
    if let Some(v) = prior.iso_max {
        map.insert("iso_max".into(), json!(v));
    }
    if let Some(v) = prior.shutter_speed_min_sec {
        map.insert("shutter_speed_min_sec".into(), json!(v));
    }
    if let Some(v) = prior.shutter_speed_max_sec {
        map.insert("shutter_speed_max_sec".into(), json!(v));
    }
    if let Some(v) = &prior.orientation {
        map.insert("orientation".into(), json!(v));
    }
    if let Some(v) = prior.has_people {
        map.insert("has_people".into(), json!(v));
    }
    if let Some(v) = prior.min_faces {
        map.insert("min_faces".into(), json!(v));
    }
    if let Some(v) = prior.max_faces {
        map.insert("max_faces".into(), json!(v));
    }
    if let Some(v) = &prior.sort_by {
        map.insert("sort_by".into(), json!(v));
    }
    if !prior.tags.is_empty() {
        map.insert("tags".into(), json!(prior.tags));
    }
    if let Some(v) = prior.kind {
        map.insert("kind".into(), json!(v));
    }
    if let Some(v) = prior.exclude_kind {
        map.insert("exclude_kind".into(), json!(v));
    }
    if let Some(v) = prior.favorite {
        map.insert("favorite".into(), json!(v));
    }
    if let Some(ms) = prior.date_from_ms {
        if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(ms) {
            map.insert("date_from".into(), json!(dt.format("%Y-%m-%d").to_string()));
        }
    }
    if let Some(ms) = prior.date_to_ms {
        if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(ms) {
            map.insert("date_to".into(), json!(dt.format("%Y-%m-%d").to_string()));
        }
    }
    serde_json::to_string(&Value::Object(map)).unwrap_or_else(|_| "{}".into())
}

fn build_router_system_prompt(tier: ChatTier, input: &ChatTurnInput) -> String {
    let prior_json = compact_prior_filters_json(&input.prior_filters);
    let today = input.current_date_utc.date_naive();
    let cur_y = today.year();
    let cur_m = today.month();
    let (this_m_from, this_m_to) = calendar_month_range(cur_y, cur_m);
    let (prev1_y, prev1_m) = shift_calendar_month(cur_y, cur_m, -1);
    let (last_m_from, last_m_to) = calendar_month_range(prev1_y, prev1_m);
    let (prev2_y, prev2_m) = shift_calendar_month(cur_y, cur_m, -2);
    let (two_m_from, two_m_to) = calendar_month_range(prev2_y, prev2_m);

    let yesterday = today - chrono::Duration::days(1);
    let days_from_mon = today.weekday().num_days_from_monday() as i64;
    let this_week_start = today - chrono::Duration::days(days_from_mon);
    let last_week_start = today - chrono::Duration::days(7);
    let last_30_days_start = today - chrono::Duration::days(30);
    let (last_wknd_sat, last_wknd_sun) = last_weekend_dates(today);

    // Keep Tier 1 (Fast 2B) entity lists compact so the small model stays laser-focused.
    let entity_cap = if tier == ChatTier::Tier1 { 15 } else { 25 };

    let date_span = match &input.library.date_range {
        Some((lo, hi)) => format!("{lo} to {hi}"),
        None => "unknown".to_string(),
    };
    let people_overview: Vec<String> = if !input.library.person_counts.is_empty() {
        input
            .library
            .person_counts
            .iter()
            .take(entity_cap)
            .map(|(name, cnt)| format!("{name} ({cnt})"))
            .collect()
    } else {
        input
            .known_person_names
            .iter()
            .take(entity_cap)
            .cloned()
            .collect()
    };
    let folders_overview: Vec<String> = input
        .library
        .known_folders
        .iter()
        .take(entity_cap)
        .cloned()
        .collect();
    let cameras_overview: Vec<String> = if !input.library.camera_counts.is_empty() {
        input
            .library
            .camera_counts
            .iter()
            .take(entity_cap)
            .map(|(name, cnt)| format!("{name} ({cnt})"))
            .collect()
    } else {
        input
            .known_cameras
            .iter()
            .take(entity_cap)
            .cloned()
            .collect()
    };
    let lenses_overview: Vec<String> = if !input.library.lens_counts.is_empty() {
        input
            .library
            .lens_counts
            .iter()
            .take(entity_cap)
            .map(|(name, cnt)| format!("{name} ({cnt})"))
            .collect()
    } else {
        input
            .known_lenses
            .iter()
            .take(entity_cap)
            .cloned()
            .collect()
    };
    let kinds_overview: Vec<String> = input
        .library
        .kind_counts
        .iter()
        .map(|(k, cnt)| format!("{k}: {cnt}"))
        .collect();
    let places_overview: Vec<String> = input
        .library
        .top_places
        .iter()
        .take(12)
        .map(|(p, cnt)| format!("{p} ({cnt})"))
        .collect();
    let years_overview: Vec<String> = input
        .library
        .year_counts
        .iter()
        .take(12)
        .map(|(yr, cnt)| format!("{yr} ({cnt})"))
        .collect();
    let months_overview: Vec<String> = input
        .library
        .top_months
        .iter()
        .take(8)
        .map(|(m, cnt)| format!("{m} ({cnt})"))
        .collect();

    // Static instructions FIRST so llama-server reuses the KV prompt cache (>90% prefix hit) across turns!
    format!(
        "You are TruongPhoto's private offline photo library assistant and query router. Convert the user's message into a compact RouterPlan JSON object. ALWAYS include the 7 required keys (intent, semantic_query, person_names, location_query, clear_prior_filters, assistant_reply, follow_up_suggestions), and include optional EXIF/date/sort/camera/lens/has_people/faces/exclude/ocr/kind/favorite keys whenever relevant (omit unused optional keys for speed).\n\
         Examples:\n\
         - \"show me photos of Sebastian\" -> {{\"intent\":\"search\",\"semantic_query\":null,\"person_names\":[\"Sebastian\"],\"location_query\":null,\"clear_prior_filters\":true,\"assistant_reply\":\"Here are your photos of Sebastian.\",\"follow_up_suggestions\":[\"Solo photos of Sebastian\",\"Sebastian at Portland Zoo\"]}}\n\
         - \"just the vertical ones\" (narrowing prior filters) -> {{\"intent\":\"search\",\"semantic_query\":null,\"person_names\":[],\"location_query\":null,\"orientation\":\"portrait\",\"clear_prior_filters\":false,\"assistant_reply\":\"Narrowing to vertical photos.\",\"follow_up_suggestions\":[\"Show favorites only\",\"Clear orientation filter\"]}}\n\
         - \"photos shot with 56mm f/1.4\" -> {{\"intent\":\"search\",\"semantic_query\":null,\"person_names\":[],\"location_query\":null,\"focal_length_min_mm\":56,\"focal_length_max_mm\":56,\"aperture_min_f\":1.4,\"aperture_max_f\":1.4,\"clear_prior_filters\":true,\"assistant_reply\":\"Here are your 56mm f/1.4 shots.\",\"follow_up_suggestions\":[\"Photos at 12mm\",\"Widest aperture shots\"]}}\n\
         - \"cool pics\" / \"best photos\" -> {{\"intent\":\"search\",\"semantic_query\":null,\"person_names\":[],\"location_query\":null,\"sort_by\":\"best\",\"clear_prior_filters\":true,\"assistant_reply\":\"Here are standout highlights from your library.\",\"follow_up_suggestions\":[\"Favorites only\",\"Wide-open portraits\"]}}\n\
         - \"zoo pics not with Sebastian\" -> {{\"intent\":\"search\",\"semantic_query\":null,\"person_names\":[],\"exclude_person_names\":[\"Sebastian\"],\"location_query\":\"Portland Zoo\",\"clear_prior_filters\":true,\"assistant_reply\":\"Here are Portland Zoo photos without Sebastian.\",\"follow_up_suggestions\":[\"Solo photos of Sebastian\",\"Animals at Portland Zoo\"]}}\n\
         - \"solo photo of Sebastian\" -> {{\"intent\":\"search\",\"semantic_query\":null,\"person_names\":[\"Sebastian\"],\"max_faces\":1,\"location_query\":null,\"clear_prior_filters\":true,\"assistant_reply\":\"Here are solo photos of Sebastian.\",\"follow_up_suggestions\":[\"Group shots with Sebastian\",\"Wide-open portraits of Sebastian\"]}}\n\
         - \"who are in my pics\" -> {{\"intent\":\"answer\",\"semantic_query\":null,\"person_names\":[],\"location_query\":null,\"clear_prior_filters\":true,\"assistant_reply\":\"Your library features ...\",\"follow_up_suggestions\":[\"Show me photos of ...\"]}}\n\
         Rules:\n\
         - Choose \"intent\" FIRST:\n\
           * \"intent\": \"answer\" for ANY conversational message or meta-question ABOUT the library itself (e.g. \"who are in my pics\", \"what's in my library\", \"tell me about my photos\", \"what trips or folders do I have\", \"how many photos do I have\", \"what is my most used lens\", \"which month did I shoot the most\", \"do I have any videos\", \"what gear do I use\", \"when were my photos taken\", \"hi\", \"what can you do?\"). Answer concretely in \"assistant_reply\" using the exact names, counts, dates, folders, places, and gear in Library Overview below, set clear_prior_filters=true, and omit ALL filter keys so NO search runs.\n\
           * \"intent\": \"search\" whenever the user wants to view, browse, or find photos/videos (or asks \"how many photos of <specific person/place/subject>\").\n\
         - Vague & exploratory photo requests (intent=\"search\"):\n\
           * For subjective/open browsing (\"cool pics\", \"best photos\", \"highlights\", \"something interesting\", \"surprise me\", \"good shots\", \"memories\"), omit \"semantic_query\" and \"has_people\", and set \"sort_by\": \"best\" (or \"capture_desc\" for \"recent\", \"latest\", \"what did I shoot recently\", \"everything\", \"my photos\"). For concrete visual subjects (\"animals\", \"nature\", \"mountain\"), omit \"sort_by\" and \"has_people\". NEVER put subjective filler words (\"cool\", \"nice\", \"best\", \"good\", \"awesome\", \"interesting\", \"random\", \"stuff\", \"pics\", \"photos\") into \"semantic_query\".\n\
           * For broad people requests without a specific name (\"show me people\", \"show me my friends\", \"pics with people\"), set \"has_people\": true and omit \"person_names\", \"semantic_query\", and \"sort_by\".\n\
         - Folders, Events & Named Places (\"location_query\" / \"exclude_location_query\"):\n\
           * Match BOTH GPS places AND folder/event names! NEVER include the trailing \"(count)\" number from Library Overview (output \"Portland Zoo\", NEVER \"Portland Zoo (146)\"). Check Folders / Events & Top places below: if the user asks for a trip, folder, or event (e.g. \"graduation stuff\", \"zoo pics\"), put the clean folder/place name in \"location_query\" and omit \"semantic_query\" UNLESS they also asked for a visual subject inside it.\n\
           * NEVER put generic terrain (\"mountain\", \"beach\", \"forest\", \"lake\", \"waterfall\", \"sunset\") into \"location_query\" unless it matches a named place/folder.\n\
         - Negations & Exclusions: Use \"exclude_location_query\" (\"not at the zoo\", \"except graduation\"), \"exclude_person_names\" (\"without Sebastian\"), \"exclude_kind\" (\"no videos\" -> \"video\"), or \"has_people\": false (\"without people\", \"empty landscapes\"). NEVER put negations into \"semantic_query\".\n\
         - Visual subjects (\"semantic_query\"): Put ONLY positive visual subjects, animals, objects, scenes, styles, or moods (expand shorthand like \"b&w\" -> \"black and white monochrome\", \"cozy\" -> \"cozy warm indoor lighting\", \"dramatic\" -> \"dramatic moody high contrast lighting\"). NEVER put EXIF adjectives (\"high ISO\", \"wide open\", \"telephoto\") into \"semantic_query\".\n\
         - Text in images (\"ocr_terms\"): Put specific literal words/phrases inside an image (e.g. \"receipt saying Target\" -> \"Target\"). For general \"photos with signs or readable text\", put \"sign or banner with readable text\" in \"semantic_query\".\n\
         - People & Face Counts: Match named people (including typos/first names) against People below WITHOUT the \"(count)\" suffix. NEVER list all people unless asked for a group photo of all of them. Use \"max_faces\": 1 for \"solo\" / \"alone\" / \"just <Name>\"; \"min_faces\": 2, \"max_faces\": 2 for \"couple\" / \"two people\"; \"min_faces\": 3 for \"group shot\" / \"group photo\".\n\
         - Superlatives & sorting (\"sort_by\"): \"best / highlights\" -> \"best\"; \"widest angle\" -> \"focal_asc\"; \"most zoomed in / longest focal\" -> \"focal_desc\"; \"highest ISO\" -> \"iso_desc\"; \"lowest ISO / cleanest\" -> \"iso_asc\"; \"widest aperture\" -> \"aperture_asc\"; \"fastest shutter\" -> \"shutter_asc\"; \"longest exposure / slowest shutter\" -> \"shutter_desc\"; \"longest video\" -> \"duration_desc\"; \"oldest\" -> \"capture_asc\"; \"newest / most recent\" -> \"capture_desc\".\n\
         - Camera, Orientation & EXIF (ONLY include when explicitly mentioned; NEVER copy \"(count)\" suffixes; NEVER put focal/lens/aperture/ISO/shutter into \"camera_model\"; \"close up\" is NOT an aperture):\n\
           * \"camera_model\": body only (e.g. \"Canon R7\" -> \"Canon EOS R7\").\n\
           * \"lens_model\": lens only (match Known lenses below).\n\
           * \"focal_length_min_mm\" / \"focal_length_max_mm\": exact \"12mm\" -> both 12; \"under 15mm\" -> max 15; \"over 300mm\" -> min 300.\n\
           * \"aperture_min_f\" / \"aperture_max_f\": exact \"f/1.4\" -> both 1.4; \"wide open / bokeh / shallow\" without a number -> max 2.0 only; \"f/8 or narrower\" -> min 8.0 only.\n\
           * \"iso_min\" / \"iso_max\": exact \"ISO 100\" -> both 100; \"ISO 3200+\" / \"grainy\" / \"high ISO\" -> min 3200 only.\n\
           * \"shutter_speed_min_sec\" / \"shutter_speed_max_sec\": decimal seconds! Faster = smaller (\"1/1000s or faster\" -> max 0.001); slower/long exposure = larger (\"long exposure / silky water\" -> min 0.05 only).\n\
           * \"orientation\": \"portrait\" (vertical/tall/phone wallpaper), \"landscape\" (horizontal/desktop wallpaper), \"square\" (square/1:1), or \"panorama\" (panoramic/ultrawide pano).\n\
         - Dates: output ISO \"date_from\" / \"date_to\" (\"YYYY-MM-DD\"). Use Date reference below for \"today\", \"yesterday\", \"this week\", \"last weekend\", \"last week\", \"last 30 days\", \"this month\", \"last month\", \"two months ago\", \"this year\", \"last year\"; open-ended bounds like \"before October\" -> date_to only, \"after September\" -> date_from only; standalone month like \"in June\" -> that month in the most recent past/current year.\n\
         - Omit \"tags\" unless user explicitly asks for a tag (\"tagged X\" / \"#X\"). Omit \"kind\" unless explicitly asking for only videos/raw/live/motion/still photos. Omit \"favorite\" unless explicitly asking for favorites/starred.\n\
         - Follow-ups: Set clear_prior_filters=false ONLY when narrowing prior active filters (e.g. \"just the vertical ones\", \"only favorites\"). Set clear_prior_filters=true when switching topic, swapping person (\"what about AJ?\" -> re-emit non-person prior filters with new person), starting over, or removing a filter.\n\
         - Write a crisp, natural \"assistant_reply\" (1–2 sentences for answers, 1 sentence for searches) and 2–3 short grounded \"follow_up_suggestions\". Never invent file paths or extra JSON keys.\n\
         Library Overview (RBAC-visible):\n\
         - Today: {} | \"yesterday\": {} | \"this week\": {}..{} | \"last weekend\": {}..{} | \"last week\": {}..{} | \"last 30 days\": {}..{}\n\
         - Calendar: \"this month\": {}..{} | \"last month\": {}..{} | \"two months ago\": {}..{} | \"this year\": {}-01-01..{}-12-31 | \"last year\": {}-01-01..{}-12-31\n\
         - Total items: {} | Favorites: {} | Kinds: {:?} | Capture span: {}\n\
         - Years: {:?}\n\
         - Top months: {:?}\n\
         - Folders / Events: {:?}\n\
         - Top GPS places: {:?}\n\
         - People (photo counts): {:?}\n\
         - Cameras: {:?}\n\
         - Lenses: {:?}\n\
         - Top tags: {:?}\n\
         - Prior active filters: {}",
        input.current_date_utc.format("%A, %Y-%m-%d"),
        yesterday.format("%Y-%m-%d"),
        this_week_start.format("%Y-%m-%d"),
        today.format("%Y-%m-%d"),
        last_wknd_sat.format("%Y-%m-%d"),
        last_wknd_sun.format("%Y-%m-%d"),
        last_week_start.format("%Y-%m-%d"),
        today.format("%Y-%m-%d"),
        last_30_days_start.format("%Y-%m-%d"),
        today.format("%Y-%m-%d"),
        this_m_from,
        this_m_to,
        last_m_from,
        last_m_to,
        two_m_from,
        two_m_to,
        cur_y,
        cur_y,
        cur_y - 1,
        cur_y - 1,
        input.library.total_assets,
        input.library.favorite_count,
        kinds_overview,
        date_span,
        years_overview,
        months_overview,
        folders_overview,
        places_overview,
        people_overview,
        cameras_overview,
        lenses_overview,
        input.library.top_tags,
        prior_json,
    )
}

fn last_weekend_dates(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let days_from_mon = today.weekday().num_days_from_monday() as i64;
    // If today is Sun (6), last completed weekend is prev Sat..Sun (today - 8 .. today - 7)?
    // Actually if today is Mon(0)..Sat(5), most recent Saturday is today - (days_from_mon + 2) days.
    // If today is Sun(6), most recent Sat..Sun is yesterday..today (today - 1 .. today).
    if days_from_mon == 6 {
        (today - chrono::Duration::days(1), today)
    } else {
        let sat = today - chrono::Duration::days(days_from_mon + 2);
        let sun = sat + chrono::Duration::days(1);
        (sat, sun)
    }
}

fn clear_all_search_filters(plan: &mut RouterPlan) {
    plan.semantic_query = None;
    plan.ocr_terms = None;
    plan.person_names.clear();
    plan.exclude_person_names.clear();
    plan.location_query = None;
    plan.exclude_location_query = None;
    plan.camera_model = None;
    plan.lens_model = None;
    plan.focal_length_min_mm = None;
    plan.focal_length_max_mm = None;
    plan.aperture_min_f = None;
    plan.aperture_max_f = None;
    plan.iso_min = None;
    plan.iso_max = None;
    plan.shutter_speed_min_sec = None;
    plan.shutter_speed_max_sec = None;
    plan.orientation = None;
    plan.has_people = None;
    plan.min_faces = None;
    plan.max_faces = None;
    plan.sort_by = None;
    plan.tags.clear();
    plan.kind = None;
    plan.exclude_kind = None;
    plan.favorite = None;
    plan.date_from_ms = None;
    plan.date_to_ms = None;
}

pub(crate) fn plan_has_any_criteria(plan: &RouterPlan) -> bool {
    plan.semantic_query.is_some()
        || plan.ocr_terms.is_some()
        || !plan.person_names.is_empty()
        || !plan.exclude_person_names.is_empty()
        || plan.location_query.is_some()
        || plan.exclude_location_query.is_some()
        || plan.camera_model.is_some()
        || plan.lens_model.is_some()
        || plan.focal_length_min_mm.is_some()
        || plan.focal_length_max_mm.is_some()
        || plan.aperture_min_f.is_some()
        || plan.aperture_max_f.is_some()
        || plan.iso_min.is_some()
        || plan.iso_max.is_some()
        || plan.shutter_speed_min_sec.is_some()
        || plan.shutter_speed_max_sec.is_some()
        || plan.orientation.is_some()
        || plan.has_people.is_some()
        || plan.min_faces.is_some()
        || plan.max_faces.is_some()
        || plan.sort_by.is_some()
        || !plan.tags.is_empty()
        || plan.kind.is_some()
        || plan.exclude_kind.is_some()
        || plan.favorite.is_some()
        || plan.date_from_ms.is_some()
        || plan.date_to_ms.is_some()
}

/// Removes subjective filler words and photo-wrapper nouns from `semantic_query` so SigLIP2
/// receives clean visual concepts (e.g. `"cool mountain pics"` -> `"mountain"`, `"cool pics"` -> `None`)
/// while preserving capitalized proper nouns like `"Nice, France"` or `"Best Buy"`.
pub(crate) fn strip_semantic_filler(raw: &str) -> Option<String> {
    const SUBJECTIVE_FILLERS: &[&str] = &[
        "cool",
        "nice",
        "good",
        "great",
        "best",
        "awesome",
        "amazing",
        "pretty",
        "interesting",
        "random",
        "neat",
        "fun",
        "favorite",
        "favourite",
        "favorites",
        "favourites",
        "standout",
        "highlight",
        "highlights",
        "surprise",
        "recent",
        "recently",
        "latest",
        "newest",
        "memories",
        "throwback",
        "vibes",
        "aesthetic",
        "epic",
        "stunning",
        "stuff",
        "things",
        "something",
        "anything",
        "everything",
    ];
    const EDGE_WRAPPERS: &[&str] = &[
        "show", "me", "find", "my", "some", "any", "all", "the", "a", "an", "of", "pics", "pic",
        "photos", "photo", "pictures", "picture", "shots", "shot", "images", "image", "ones",
        "one", "trip", "trips", "now", "from", "that", "those", "these",
    ];

    let raw_words: Vec<&str> = raw
        .split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'' && c != '&')
        })
        .filter(|t| !t.is_empty())
        .collect();

    let mut tokens: Vec<&str> = Vec::with_capacity(raw_words.len());
    for (idx, &t) in raw_words.iter().enumerate() {
        let low = t.to_ascii_lowercase();
        let starts_upper = t.chars().next().is_some_and(|c| c.is_uppercase());
        // Preserve proper nouns like "Best Buy" or "Nice France" when capitalized alongside another capitalized word or non-filler.
        let looks_like_proper_noun = starts_upper
            && raw_words.len() >= 2
            && ((idx + 1 < raw_words.len()
                && raw_words[idx + 1]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_uppercase()))
                || (idx > 0
                    && raw_words[idx - 1]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_uppercase())));
        if !looks_like_proper_noun && SUBJECTIVE_FILLERS.contains(&low.as_str()) {
            continue;
        }
        tokens.push(t);
    }

    while let Some(first) = tokens.first() {
        if EDGE_WRAPPERS.contains(&first.to_ascii_lowercase().as_str()) {
            tokens.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = tokens.last() {
        if EDGE_WRAPPERS.contains(&last.to_ascii_lowercase().as_str()) {
            tokens.pop();
        } else {
            break;
        }
    }

    if tokens.is_empty() {
        return None;
    }

    // Expand photographic shorthand ("b&w" / "bw" -> "black and white monochrome") for SigLIP2.
    let joined = tokens.join(" ");
    let low_joined = joined.to_ascii_lowercase();
    if matches!(low_joined.as_str(), "b&w" | "bw" | "b/w" | "black & white") {
        Some("black and white monochrome".to_string())
    } else {
        Some(joined)
    }
}

/// Word-boundary check on an already-lowercased string so short tokens like `"mm"`, `"sec"`,
/// `"top"`, `"nice"`, `"cool"`, or `"tall"` never match inside `"summer"`, `"second"`, `"rooftop"`,
/// `"Venice"`, `"school"`, or `"install"`.
pub(crate) fn has_word(lower: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = lower[from..].find(word) {
        let start = from + rel;
        let end = start + word.len();
        let left_ok = lower[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let right_ok = lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Checks whether a lowercase string contains a numeric measurement with unit suffix like `12mm`, `50mm`, `0.5s`, `2sec`.
fn has_numeric_unit(lower: &str, suffix: &str) -> bool {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .any(|tok| {
            tok.strip_suffix(suffix).is_some_and(|num| {
                !num.is_empty()
                    && num.chars().all(|c| c.is_ascii_digit() || c == '.')
                    && num.chars().any(|c| c.is_ascii_digit())
            })
        })
}

/// Deterministic single-filter removal for turns like `"remove the location filter"`,
/// `"remove the Location: Portland Zoo filter"`, or `"drop the date filter"`.
pub(crate) fn try_apply_filter_removal(
    lower: &str,
    prior: &RouterPlan,
    plan: &mut RouterPlan,
) -> bool {
    // Handle negation-undo phrasing ("actually include videos too", "include non-favorites")
    let include_videos_undo = (lower.contains("include video")
        || lower.contains("with videos too")
        || lower.contains("include videos too"))
        && prior.exclude_kind == Some(AssetKind::Video);
    let include_non_fav_undo = (lower.contains("include non-fav")
        || lower.contains("not just fav")
        || lower.contains("not only fav"))
        && prior.favorite == Some(true);
    if (include_videos_undo || include_non_fav_undo) && plan_has_any_criteria(prior) {
        let mut updated = prior.clone();
        updated.intent = Some("search".to_string());
        updated.clear_prior_filters = true;
        updated.assistant_reply = plan.assistant_reply.clone();
        updated.follow_up_suggestions = plan.follow_up_suggestions.clone();
        if include_videos_undo {
            updated.exclude_kind = None;
            if updated.kind == Some(AssetKind::Photo) {
                updated.kind = None;
            }
        }
        if include_non_fav_undo {
            updated.favorite = None;
        }
        *plan = updated;
        return true;
    }

    let is_removal = lower.contains("remove the")
        || lower.contains("remove filter")
        || lower.starts_with("remove ")
        || lower.contains("drop the")
        || lower.starts_with("drop ")
        || lower.contains("clear the");
    if !is_removal || !plan_has_any_criteria(prior) {
        return false;
    }

    // Extract the filter target label AND optional value after `:` (e.g., `"remove the Person: Sebastian filter"`).
    let (target_raw, value_raw) = match lower.split_once(':') {
        Some((lhs, rhs)) => (
            lhs,
            Some(
                rhs.replace("filter", "")
                    .replace("from my search", "")
                    .replace("from the search", "")
                    .replace("from search", "")
                    .replace("from my photos", "")
                    .trim()
                    .to_string(),
            ),
        ),
        None => (lower, None),
    };
    let target_val = value_raw
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let target_clause = target_raw
        .replace("from my search", "")
        .replace("from the search", "")
        .replace("from search", "")
        .replace("from my photos", "")
        .replace("from photos", "");
    let target_clause = target_clause.as_str();

    let mut updated = prior.clone();
    updated.intent = Some("search".to_string());
    updated.clear_prior_filters = true;
    updated.assistant_reply = plan.assistant_reply.clone();
    updated.follow_up_suggestions = plan.follow_up_suggestions.clone();

    let mut removed_any = false;
    if has_word(target_clause, "query")
        || has_word(target_clause, "subject")
        || has_word(target_clause, "scene")
        || lower.contains("remove the search:")
        || lower.contains("remove the search filter")
        || lower.contains("drop the search filter")
        || lower.contains("clear the search filter")
    {
        updated.semantic_query = None;
        removed_any = true;
    }
    if has_word(target_clause, "location")
        || has_word(target_clause, "place")
        || has_word(target_clause, "folder")
    {
        updated.location_query = None;
        removed_any = true;
    }
    if has_word(target_clause, "person") || has_word(target_clause, "people") {
        if let Some(val) = target_val {
            if !matches!(val, "present" | "none" | "any") && !updated.person_names.is_empty() {
                updated
                    .person_names
                    .retain(|p| p.trim().to_ascii_lowercase() != val);
            } else {
                updated.person_names.clear();
                updated.has_people = None;
            }
        } else {
            updated.person_names.clear();
            updated.has_people = None;
        }
        removed_any = true;
    }
    if has_word(target_clause, "date")
        || has_word(target_clause, "month")
        || has_word(target_clause, "year")
        || has_word(target_clause, "until")
        || lower.contains("remove the from ")
        || lower.contains("drop the from ")
        || lower.contains("clear the from ")
    {
        updated.date_from_ms = None;
        updated.date_to_ms = None;
        removed_any = true;
    }
    if has_word(target_clause, "camera") {
        updated.camera_model = None;
        removed_any = true;
    }
    if has_word(target_clause, "lens") {
        updated.lens_model = None;
        removed_any = true;
    }
    if has_word(target_clause, "focal") || has_word(target_clause, "mm") {
        updated.focal_length_min_mm = None;
        updated.focal_length_max_mm = None;
        removed_any = true;
    }
    if has_word(target_clause, "aperture") || target_clause.contains("f/") {
        updated.aperture_min_f = None;
        updated.aperture_max_f = None;
        removed_any = true;
    }
    if has_word(target_clause, "iso") {
        updated.iso_min = None;
        updated.iso_max = None;
        removed_any = true;
    }
    if has_word(target_clause, "shutter") || has_word(target_clause, "exposure") {
        updated.shutter_speed_min_sec = None;
        updated.shutter_speed_max_sec = None;
        removed_any = true;
    }
    if has_word(target_clause, "orientation")
        || has_word(target_clause, "portrait")
        || has_word(target_clause, "vertical")
        || has_word(target_clause, "landscape")
        || has_word(target_clause, "horizontal")
        || has_word(target_clause, "square")
        || has_word(target_clause, "panorama")
    {
        updated.orientation = None;
        removed_any = true;
    }
    if has_word(target_clause, "faces")
        || has_word(target_clause, "solo")
        || has_word(target_clause, "group")
        || has_word(target_clause, "couple")
    {
        updated.min_faces = None;
        updated.max_faces = None;
        removed_any = true;
    }
    if has_word(target_clause, "exclude")
        || has_word(target_clause, "excluding")
        || has_word(target_clause, "exclusion")
        || has_word(target_clause, "without")
        || has_word(target_clause, "not")
    {
        if let Some(val) = target_val {
            let before_people = updated.exclude_person_names.len();
            updated
                .exclude_person_names
                .retain(|p| p.trim().to_ascii_lowercase() != val);
            let removed_person = updated.exclude_person_names.len() < before_people;
            let removed_loc = updated
                .exclude_location_query
                .as_deref()
                .is_some_and(|l| l.trim().to_ascii_lowercase() == val);
            if removed_loc {
                updated.exclude_location_query = None;
            }
            let removed_kind = updated
                .exclude_kind
                .is_some_and(|k| val.contains(k.as_str()));
            if removed_kind {
                updated.exclude_kind = None;
            }
            if !removed_person && !removed_loc && !removed_kind {
                updated.exclude_person_names.clear();
                updated.exclude_location_query = None;
                updated.exclude_kind = None;
            }
        } else {
            updated.exclude_person_names.clear();
            updated.exclude_location_query = None;
            updated.exclude_kind = None;
        }
        removed_any = true;
    }
    if has_word(target_clause, "sort") {
        updated.sort_by = None;
        removed_any = true;
    }
    if has_word(target_clause, "tag") || has_word(target_clause, "tags") {
        if let Some(val) = target_val {
            let v_clean = val.trim_start_matches('#');
            updated
                .tags
                .retain(|t| t.trim().to_ascii_lowercase() != v_clean);
        } else {
            updated.tags.clear();
        }
        removed_any = true;
    }
    if has_word(target_clause, "kind")
        || has_word(target_clause, "video")
        || has_word(target_clause, "videos")
        || has_word(target_clause, "photo")
        || has_word(target_clause, "photos")
        || has_word(target_clause, "raw")
    {
        updated.kind = None;
        updated.exclude_kind = None;
        removed_any = true;
    }
    if has_word(target_clause, "favorite")
        || has_word(target_clause, "favorites")
        || has_word(target_clause, "favourite")
        || has_word(target_clause, "favourites")
        || has_word(target_clause, "starred")
    {
        updated.favorite = None;
        removed_any = true;
    }
    if has_word(target_clause, "ocr") || has_word(target_clause, "text") {
        updated.ocr_terms = None;
        removed_any = true;
    }

    // Fallback: `"remove Sebastian"` directly naming a prior person filter
    if !removed_any && !updated.person_names.is_empty() {
        let before = updated.person_names.len();
        updated.person_names.retain(|p| {
            let first = p
                .split_whitespace()
                .next()
                .unwrap_or(p)
                .to_ascii_lowercase();
            !has_word(target_clause, &p.to_ascii_lowercase()) && !has_word(target_clause, &first)
        });
        if updated.person_names.len() < before {
            removed_any = true;
        }
    }

    if removed_any {
        *plan = updated;
        true
    } else {
        false
    }
}

/// Strips count suffix `"CS Grad Pics 2026 (236)"` -> `"CS Grad Pics 2026"`.
pub(crate) fn strip_folder_count(entry: &str) -> &str {
    entry
        .rsplit_once(" (")
        .map(|(name, _)| name.trim())
        .unwrap_or_else(|| entry.trim())
}

pub(crate) fn extract_explicit_year(lower: &str) -> Option<i32> {
    lower
        .split(|c: char| !c.is_ascii_digit())
        .filter(|tok| tok.len() == 4)
        .filter_map(|tok| tok.parse::<i32>().ok())
        .find(|&yr| (1970..=2099).contains(&yr))
}

pub(crate) fn season_date_range_ms(lower: &str, now_utc: DateTime<Utc>) -> Option<(i64, i64)> {
    let today = now_utc.date_naive();
    let y = today.year();
    let m = today.month();
    let explicit_year = extract_explicit_year(lower);
    let wants_last = lower.contains("last summer")
        || lower.contains("last spring")
        || lower.contains("last fall")
        || lower.contains("last autumn")
        || lower.contains("last winter");
    let (start, end) = if has_word(lower, "summer") {
        let sy = explicit_year.unwrap_or(if wants_last {
            if m > 8 {
                y
            } else {
                y - 1
            }
        } else if m >= 6 {
            y
        } else {
            y - 1
        });
        (
            NaiveDate::from_ymd_opt(sy, 6, 1)?,
            NaiveDate::from_ymd_opt(sy, 8, 31)?,
        )
    } else if has_word(lower, "spring") {
        let sy = explicit_year.unwrap_or(if wants_last {
            if m > 5 {
                y
            } else {
                y - 1
            }
        } else if m >= 3 {
            y
        } else {
            y - 1
        });
        (
            NaiveDate::from_ymd_opt(sy, 3, 1)?,
            NaiveDate::from_ymd_opt(sy, 5, 31)?,
        )
    } else if has_word(lower, "fall") || has_word(lower, "autumn") {
        let sy = explicit_year.unwrap_or(if wants_last {
            if m > 11 {
                y
            } else {
                y - 1
            }
        } else if m >= 9 {
            y
        } else {
            y - 1
        });
        (
            NaiveDate::from_ymd_opt(sy, 9, 1)?,
            NaiveDate::from_ymd_opt(sy, 11, 30)?,
        )
    } else if has_word(lower, "winter") {
        let end_y = if let Some(ey) = explicit_year {
            ey
        } else if wants_last {
            if m >= 3 {
                y
            } else {
                y - 1
            }
        } else if m == 12 {
            y + 1
        } else {
            y
        };
        let feb_last = NaiveDate::from_ymd_opt(end_y, 3, 1)?.pred_opt()?;
        (NaiveDate::from_ymd_opt(end_y - 1, 12, 1)?, feb_last)
    } else {
        return None;
    };
    let s_dt = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0)?);
    let e_dt = Utc.from_utc_datetime(&end.and_hms_milli_opt(23, 59, 59, 999)?);
    Some((s_dt.timestamp_millis(), e_dt.timestamp_millis()))
}

/// Shared classifier for library-overview / conversational meta-questions (used by both
/// Tier 0 Classic and Tiers 1–3 sidecar sanitizer).
pub(crate) fn is_meta_library_question(lower: &str) -> bool {
    let trimmed_punct = lower.trim_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace());
    let is_short_greeting = matches!(
        trimmed_punct,
        "hi" | "hello" | "hey" | "yo" | "sup" | "thanks" | "thank you" | "help"
    );
    if is_short_greeting {
        return true;
    }

    // Explicit library-fact questions win even when phrased as "show me what cameras I use"
    // or "show me who is in my library".
    const STRICT_META_PHRASES: &[&str] = &[
        "who are in my",
        "who is in my",
        "who's in my",
        "whos in my",
        "who are the people",
        "who appears",
        "who is in the most",
        "most photographed",
        "who do you know",
        "who do i have",
        "what people",
        "which people",
        "list the people",
        "list people",
        "people in my library",
        "what's in my library",
        "whats in my library",
        "what is in my library",
        "what's in my photo",
        "whats in my photo",
        "what is in my photo",
        "what's in my pic",
        "whats in my pic",
        "what is in my pic",
        "tell me about my",
        "summarize my",
        "overview of my",
        "describe my library",
        "what folder",
        "which folder",
        "list folder",
        "what trip",
        "what event",
        "what album",
        "what collection",
        "where have i been",
        "where do i take",
        "where do i shoot",
        "favorite place",
        "favourite place",
        "most visited",
        "what places are in my",
        "what camera",
        "which camera",
        "what lens",
        "which lens",
        "most used lens",
        "most used camera",
        "favorite lens",
        "favourite lens",
        "what gear",
        "what equipment",
        "which month",
        "what month",
        "which year",
        "what year",
        "when were my",
        "what dates",
        "date range",
        "what can you do",
        "how do you work",
    ];
    if STRICT_META_PHRASES.iter().any(|kw| lower.contains(kw)) {
        return true;
    }

    let is_explicit_show_search = [
        "show me",
        "show ",
        "find ",
        "pull up",
        "get me",
        "give me",
        "search ",
        "look for",
        "browse",
        "pics of",
        "photos of",
        "pictures of",
        "shots of",
        "what did i shoot",
        "what have i shot",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    !is_explicit_show_search
        && [
            "how many photo",
            "how many pic",
            "how many image",
            "how many video",
            "how many item",
            "how many file",
            "how many favorite",
            "how many favourite",
            "total photo",
            "total pic",
        ]
        .iter()
        .any(|kw| lower.contains(kw))
}

fn sanitize_sidecar_plan(
    user_message: &str,
    prior: &RouterPlan,
    known_folders: &[String],
    known_people: &[String],
    now_utc: DateTime<Utc>,
    plan: &mut RouterPlan,
) {
    let lower = user_message.trim().to_ascii_lowercase();

    // 0. Deterministic single-filter removal ("remove the location filter", clickable chip removal).
    if try_apply_filter_removal(&lower, prior, plan) {
        return;
    }

    // 1. Broad informational / meta-question guard.
    let is_explicit_show_search = [
        "show me",
        "show ",
        "find ",
        "pull up",
        "get me",
        "give me",
        "search ",
        "look for",
        "browse",
        "pics of",
        "photos of",
        "pictures of",
        "shots of",
        "what did i shoot",
        "what have i shot",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    let is_meta_info_question = is_meta_library_question(&lower);

    let has_specific_count_filter = (!plan.person_names.is_empty()
        || plan.date_from_ms.is_some()
        || plan.date_to_ms.is_some()
        || plan.location_query.is_some()
        || plan.kind.is_some()
        || plan.favorite.is_some()
        || lower.contains("how many video")
        || lower.contains("how many favorite")
        || lower.contains("how many favourite")
        || lower.contains(" of "))
        && (lower.contains("how many") || lower.contains("total "));

    if (is_meta_info_question && !has_specific_count_filter)
        || (plan.intent.as_deref() == Some("answer")
            && !is_explicit_show_search
            && !has_specific_count_filter)
    {
        plan.intent = Some("answer".to_string());
        clear_all_search_filters(plan);
        plan.clear_prior_filters = true;
        return;
    }

    plan.intent = Some("search".to_string());

    // 1b. Follow-up entity swap ("what about AJ?", "how about Sebastian?"): when prior had a
    // person filter and the user swaps to another person without naming the prior person, replace
    // `person_names` cleanly while carrying over the remaining non-person prior filters.
    let is_what_about = lower.starts_with("what about ")
        || lower.starts_with("how about ")
        || lower.contains("instead of ");
    if is_what_about && !prior.person_names.is_empty() && !plan.person_names.is_empty() {
        let prior_low: HashSet<String> = prior
            .person_names
            .iter()
            .map(|p| p.to_ascii_lowercase())
            .collect();
        let has_new_person = plan
            .person_names
            .iter()
            .any(|p| !prior_low.contains(&p.to_ascii_lowercase()));
        let mentions_prior = prior.person_names.iter().any(|p| {
            let first = p
                .split_whitespace()
                .next()
                .unwrap_or(p)
                .to_ascii_lowercase();
            !first.is_empty() && has_word(&lower, &first)
        });
        if has_new_person && !mentions_prior {
            let new_people: Vec<String> = plan
                .person_names
                .iter()
                .filter(|p| !prior_low.contains(&p.to_ascii_lowercase()))
                .cloned()
                .collect();
            let mut prior_without_people = prior.clone();
            prior_without_people.person_names.clear();
            let mut next_turn = plan.clone();
            next_turn.person_names = new_people;
            next_turn.clear_prior_filters = false;
            let mut merged = merge_router_plan(&prior_without_people, &next_turn);
            merged.intent = Some("search".to_string());
            merged.clear_prior_filters = true;
            *plan = merged;
        }
    }
    let _ = known_people;

    // Strip speculative tags when the user never asked for a tag ("tagged X" / "#X").
    // If a stripped tag matches a known folder or was the only subject cue (e.g. "highest ISO zoo photos"
    // -> tags: ["zoo"]), promote it to `location_query` so the search succeeds instead of returning 0 hits!
    let has_explicit_tag_cue = has_word(&lower, "tag")
        || has_word(&lower, "tagged")
        || has_word(&lower, "tags")
        || lower.contains('#');
    if !has_explicit_tag_cue && !plan.tags.is_empty() && prior.tags.is_empty() {
        let dropped = std::mem::take(&mut plan.tags);
        if plan.location_query.is_none() && plan.semantic_query.is_none() {
            if let Some(first_tag) = dropped.first() {
                let low_tag = first_tag.to_ascii_lowercase();
                let folder_match = known_folders
                    .iter()
                    .map(|f| strip_folder_count(f))
                    .find(|fname| fname.to_ascii_lowercase().contains(&low_tag));
                if let Some(matched_folder) = folder_match {
                    plan.location_query = Some(matched_folder.to_string());
                } else {
                    plan.semantic_query = Some(first_tag.clone());
                }
            }
        }
    }

    let has_lower_bound_cue = [
        "over ",
        "above",
        "more than",
        "or more",
        "or higher",
        "at least",
        "longer than",
        "greater than",
        "higher than",
        "narrower than",
        "or narrower",
        "or slower",
        "or longer",
        "slower than",
        "zoomed",
        "highest",
        "beyond ",
        ">=",
        ">",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
        || lower.split_whitespace().any(|w| {
            w.ends_with('+') && w.len() >= 2 && w[..w.len() - 1].chars().any(|c| c.is_ascii_digit())
        });

    let has_upper_bound_cue = [
        "under ",
        "below",
        "less than",
        "or less",
        "or lower",
        "shorter than",
        "wider than",
        "or wider",
        "or faster",
        "faster than",
        "lower than",
        "lowest",
        "at most",
        "up to ",
        "<=",
        "<",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    let has_explicit_fstop = lower.contains("f/")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .any(|tok| {
                if !tok.starts_with('f') || tok.len() < 2 {
                    return false;
                }
                let num_part = &tok[1..];
                if let Ok(v) = num_part.parse::<f64>() {
                    // Require realistic f-stop range; exclude bare "f1" (Formula 1) / "f150" (truck).
                    (num_part.contains('.') && (0.7..=64.0).contains(&v))
                        || (!num_part.contains('.')
                            && (2.0..=32.0).contains(&v)
                            && num_part != "16")
                } else {
                    false
                }
            });
    let has_wide_open_phrase = lower.contains("wide open")
        || lower.contains("wide-open")
        || lower.contains("bokeh")
        || lower.contains("shallow depth")
        || lower.contains("shallow dof");
    let has_aperture_cue = has_explicit_fstop
        || has_wide_open_phrase
        || lower.contains("aperture")
        || lower.contains("f-stop")
        || lower.contains("f stop")
        || lower.contains("depth of field");
    let inherit_prior = !plan.clear_prior_filters;
    let prior_had_aperture =
        inherit_prior && (prior.aperture_min_f.is_some() || prior.aperture_max_f.is_some());

    if !has_aperture_cue && !prior_had_aperture {
        plan.aperture_min_f = None;
        plan.aperture_max_f = None;
    } else if has_wide_open_phrase && !has_explicit_fstop {
        // "wide open" without an explicit f-stop means <= max (never exact f/2.0 == f/2.0!).
        if let Some(hi) = plan.aperture_max_f.or(plan.aperture_min_f) {
            plan.aperture_min_f = None;
            plan.aperture_max_f = Some(hi);
        }
    } else if has_aperture_cue {
        if let (Some(lo), None) = (plan.aperture_min_f, plan.aperture_max_f) {
            if !has_lower_bound_cue {
                plan.aperture_max_f = Some(lo);
            }
        } else if let (None, Some(hi)) = (plan.aperture_min_f, plan.aperture_max_f) {
            if !has_upper_bound_cue && !has_wide_open_phrase {
                plan.aperture_min_f = Some(hi);
            }
        }
    }

    let has_iso_cue = has_word(&lower, "iso")
        || has_numeric_unit(&lower, "iso")
        || lower.contains("film grain")
        || lower.contains("low light")
        || lower.contains("low-light")
        || has_word(&lower, "grainy")
        || has_word(&lower, "noisy")
        || has_word(&lower, "noise");
    let prior_had_iso = inherit_prior && (prior.iso_min.is_some() || prior.iso_max.is_some());

    if !has_iso_cue && !prior_had_iso {
        plan.iso_min = None;
        plan.iso_max = None;
    } else if has_iso_cue {
        let is_high_iso_vague = lower.contains("high iso")
            || lower.contains("film grain")
            || lower.contains("low light")
            || lower.contains("low-light")
            || has_word(&lower, "grainy")
            || has_word(&lower, "noisy")
            || has_word(&lower, "noise");
        let is_low_iso_vague =
            lower.contains("low iso") || lower.contains("base iso") || has_word(&lower, "cleanest");
        if let (Some(lo), None) = (plan.iso_min, plan.iso_max) {
            if !has_lower_bound_cue && !is_high_iso_vague {
                plan.iso_max = Some(lo);
            }
        } else if let (None, Some(hi)) = (plan.iso_min, plan.iso_max) {
            if is_high_iso_vague && !has_upper_bound_cue {
                // Small models sometimes misread "low light" as low ISO (e.g., iso_max: 200); drop it.
                plan.iso_max = None;
            } else if !has_upper_bound_cue && !is_low_iso_vague {
                plan.iso_min = Some(hi);
            }
        }
    }

    let lower_no_wide_open = lower.replace("wide open", "").replace("wide-open", "");
    let has_focal_cue = has_numeric_unit(&lower_no_wide_open, "mm")
        || has_word(&lower_no_wide_open, "mm")
        || lower_no_wide_open.contains("focal")
        || has_word(&lower_no_wide_open, "wide")
        || has_word(&lower_no_wide_open, "widest")
        || lower_no_wide_open.contains("ultrawide")
        || lower_no_wide_open.contains("ultra-wide")
        || lower_no_wide_open.contains("telephoto")
        || lower_no_wide_open.contains("zoom")
        || lower_no_wide_open.contains("fisheye");
    let prior_had_focal = inherit_prior
        && (prior.focal_length_min_mm.is_some() || prior.focal_length_max_mm.is_some());

    if !has_focal_cue && !prior_had_focal {
        plan.focal_length_min_mm = None;
        plan.focal_length_max_mm = None;
    } else if has_focal_cue {
        if let (Some(lo), None) = (plan.focal_length_min_mm, plan.focal_length_max_mm) {
            if !has_lower_bound_cue && !lower_no_wide_open.contains("telephoto") {
                plan.focal_length_max_mm = Some(lo);
            }
        } else if let (None, Some(hi)) = (plan.focal_length_min_mm, plan.focal_length_max_mm) {
            if !has_upper_bound_cue
                && !has_word(&lower_no_wide_open, "wide")
                && !lower_no_wide_open.contains("ultrawide")
                && !lower_no_wide_open.contains("ultra-wide")
                && !lower_no_wide_open.contains("fisheye")
            {
                plan.focal_length_min_mm = Some(hi);
            }
        }
    }

    let has_long_exp_phrase = lower.contains("long exposure")
        || lower.contains("slow shutter")
        || lower.contains("silky")
        || lower.contains("motion blur");
    let has_explicit_shutter_frac = lower.split_whitespace().any(|tok| {
        let cleaned = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/');
        if let Some(denom) = cleaned.strip_prefix("1/") {
            let denom = denom.trim_end_matches('s');
            !denom.contains('/') && !denom.is_empty() && denom.chars().all(|c| c.is_ascii_digit())
        } else {
            false
        }
    });
    let has_seconds_unit = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .any(|tok| {
            let Some(num) = tok.strip_suffix('s') else {
                return false;
            };
            if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit() || c == '.') {
                return false;
            }
            // Exclude decades like "60s"-"90s", "1990s", "2000s", while keeping common long exposures ("20s", "30s").
            if !num.contains('.') {
                if let Ok(v) = num.parse::<u32>() {
                    if matches!(v, 60 | 70 | 80 | 90 | 1900..=2099) {
                        return false;
                    }
                }
            }
            true
        });
    let has_shutter_cue = lower.contains("shutter")
        || lower.contains("exposure")
        || has_long_exp_phrase
        || lower.contains("freeze")
        || has_explicit_shutter_frac
        || has_numeric_unit(&lower, "sec")
        || has_seconds_unit
        || has_word(&lower, "sec")
        || has_word(&lower, "secs")
        || has_word(&lower, "seconds");
    let prior_had_shutter = inherit_prior
        && (prior.shutter_speed_min_sec.is_some() || prior.shutter_speed_max_sec.is_some());

    if !has_shutter_cue && !prior_had_shutter {
        plan.shutter_speed_min_sec = None;
        plan.shutter_speed_max_sec = None;
    } else if has_shutter_cue {
        if has_long_exp_phrase && !has_upper_bound_cue && !has_explicit_shutter_frac {
            // "long exposure / silky" means >= min (never exact 0.05..0.05 or <= 0.05!).
            if let Some(lo) = plan.shutter_speed_min_sec.or(plan.shutter_speed_max_sec) {
                plan.shutter_speed_min_sec = Some(lo);
                plan.shutter_speed_max_sec = None;
            }
        } else if let (Some(v), None) = (plan.shutter_speed_min_sec, plan.shutter_speed_max_sec) {
            if lower.contains("fast shutter")
                || lower.contains("fastest")
                || lower.contains("or faster")
                || lower.contains("freeze")
            {
                plan.shutter_speed_max_sec = Some(v);
                plan.shutter_speed_min_sec = None;
            } else if has_explicit_shutter_frac && !has_lower_bound_cue {
                // Exact shutter speed like "at 1/500s" -> tight band (min == max).
                plan.shutter_speed_max_sec = Some(v);
            }
        } else if let (None, Some(v)) = (plan.shutter_speed_min_sec, plan.shutter_speed_max_sec) {
            if has_explicit_shutter_frac && !has_upper_bound_cue && !lower.contains("faster") {
                plan.shutter_speed_min_sec = Some(v);
            }
        }
    }

    let has_orientation_cue = has_word(&lower, "vertical")
        || has_word(&lower, "horizontal")
        || has_word(&lower, "landscape")
        || has_word(&lower, "square")
        || has_word(&lower, "tall")
        || has_word(&lower, "panorama")
        || has_word(&lower, "panoramas")
        || has_word(&lower, "panoramic")
        || has_word(&lower, "pano")
        || lower.contains("wallpaper")
        || lower.contains("phone background")
        || lower.contains("lock screen")
        || lower.contains("desktop background")
        || lower.contains("portrait orientation")
        || lower.contains("portrait mode")
        || lower.contains("portrait aspect")
        || lower.contains("portrait ones")
        || lower.contains("only portrait")
        || lower.contains("portrait only")
        || lower.contains("portrait shot")
        || lower.contains("aspect ratio")
        || lower.contains("1:1");

    if !has_orientation_cue && (!inherit_prior || prior.orientation.is_none()) {
        plan.orientation = None;
    }
    if plan.orientation.is_none() {
        if has_word(&lower, "panorama")
            || has_word(&lower, "panoramas")
            || has_word(&lower, "panoramic")
            || has_word(&lower, "pano")
        {
            plan.orientation = Some("panorama".to_string());
        } else if lower.contains("phone wallpaper")
            || lower.contains("phone background")
            || lower.contains("lock screen")
        {
            plan.orientation = Some("portrait".to_string());
        } else if lower.contains("desktop wallpaper") || lower.contains("desktop background") {
            plan.orientation = Some("landscape".to_string());
        }
    }

    // Small-model (Tier 1) hallucination guards:
    // 1. Do not restrict `kind: Photo` when user casually says "photos of X" (excludes Live Photos & RAW+JPG stacks!).
    let wants_still_only = lower.contains("only photo")
        || lower.contains("photos only")
        || lower.contains("photo only")
        || lower.contains("still photo")
        || lower.contains("stills only")
        || lower.contains("no video")
        || lower.contains("not video")
        || lower.contains("without video");
    if plan.kind == Some(AssetKind::Photo)
        && !wants_still_only
        && (!inherit_prior || prior.kind != Some(AssetKind::Photo))
    {
        plan.kind = None;
    }
    if (lower.contains("no video")
        || lower.contains("not video")
        || lower.contains("without video"))
        && plan.kind.is_none()
        && plan.exclude_kind.is_none()
    {
        plan.exclude_kind = Some(AssetKind::Video);
    }

    // 2. Do not restrict `favorite: false` unless explicitly requested.
    let wants_non_favorite = lower.contains("not favorite")
        || lower.contains("not favourite")
        || lower.contains("non-favorite")
        || lower.contains("non favorite")
        || lower.contains("unfavorited")
        || lower.contains("unstarred");
    if plan.favorite == Some(false)
        && !wants_non_favorite
        && (!inherit_prior || prior.favorite != Some(false))
    {
        plan.favorite = None;
    }

    // 3. Face-count rescue ("solo", "couple", "group shot").
    if plan.min_faces.is_none() && plan.max_faces.is_none() {
        if has_word(&lower, "solo")
            || lower.contains("by themselves")
            || lower.contains("by himself")
            || lower.contains("by herself")
            || (has_word(&lower, "alone") && !plan.person_names.is_empty())
        {
            plan.max_faces = Some(1);
            if plan.person_names.is_empty() {
                plan.min_faces = Some(1);
            }
        } else if lower.contains("two people")
            || lower.contains("2 people")
            || has_word(&lower, "couple")
        {
            plan.min_faces = Some(2);
            plan.max_faces = Some(2);
        } else if lower.contains("group shot")
            || lower.contains("group photo")
            || lower.contains("group pic")
            || lower.contains("big group")
        {
            plan.min_faces = Some(3);
        }
    }

    // Superlative safety net: ensure superlatives always populate `sort_by` so they never execute
    // as empty queries.
    if plan.sort_by.is_none() {
        if lower.contains("widest angle")
            || lower.contains("widest shot")
            || lower.contains("widest photo")
        {
            plan.sort_by = Some("focal_asc".to_string());
            if plan
                .semantic_query
                .as_deref()
                .is_some_and(|s| s.to_ascii_lowercase().contains("wide"))
            {
                plan.semantic_query = None;
            }
        } else if lower.contains("most zoomed") || lower.contains("longest focal") {
            plan.sort_by = Some("focal_desc".to_string());
        } else if lower.contains("highest iso") {
            plan.sort_by = Some("iso_desc".to_string());
        } else if lower.contains("fastest shutter") {
            plan.sort_by = Some("shutter_asc".to_string());
        } else if lower.contains("longest exposure") || lower.contains("slowest shutter") {
            plan.sort_by = Some("shutter_desc".to_string());
        }
    }

    // Negation safety net: "without people" / "no people" -> has_people = Some(false).
    // Conversely, strip hallucinated `has_people: false` when the user never asked to exclude people!
    let wants_no_people = lower.contains("without people")
        || lower.contains("without any people")
        || lower.contains("no people")
        || has_word(&lower, "nobody")
        || lower.contains("empty landscape")
        || has_word(&lower, "unpeopled")
        || lower.contains("no faces")
        || lower.contains("without faces");
    if wants_no_people {
        plan.has_people = Some(false);
    } else if plan.has_people == Some(false) && (!inherit_prior || prior.has_people != Some(false))
    {
        plan.has_people = None;
    } else if plan.has_people == Some(true)
        && plan.person_names.is_empty()
        && (!inherit_prior || prior.has_people != Some(true))
    {
        let mentions_people = [
            "people",
            "person",
            "friends",
            "friend",
            "family",
            "fam",
            "portraits",
            "portrait",
            "faces",
            "face",
            "someone",
            "anyone",
            "everyone",
            "folks",
            "crowd",
            "group",
            "couple",
            "selfie",
            "selfies",
            "kiddos",
            "kids",
        ]
        .iter()
        .any(|kw| has_word(&lower, kw));
        if !mentions_people {
            plan.has_people = None;
        }
    }

    // Strip hallucinated `sort_by: "aperture_asc"` when the user did not ask for aperture/bokeh
    // or subjective/exploratory browsing (so concrete searches like "show me animals" keep natural
    // semantic or capture-date ranking).
    let wants_exploratory = [
        "cool",
        "best",
        "good",
        "great",
        "nice",
        "awesome",
        "pretty",
        "interesting",
        "surprise",
        "random",
        "highlight",
        "highlights",
        "standout",
        "top",
        "memories",
        "throwback",
        "something",
        "anything",
    ]
    .iter()
    .any(|kw| has_word(&lower, kw));
    if plan.sort_by.as_deref() == Some("aperture_asc")
        && !has_aperture_cue
        && !wants_exploratory
        && (!inherit_prior || prior.sort_by.as_deref() != Some("aperture_asc"))
    {
        plan.sort_by = None;
    }

    // Strip generic terrain/scene words that LLMs occasionally misplace into `location_query`
    // (e.g. "outdoor", "mountains", "nature", "beach") and promote them to `semantic_query`.
    const GENERIC_SCENE_LOCATIONS: &[&str] = &[
        "outdoor",
        "outdoors",
        "indoor",
        "indoors",
        "nature",
        "mountain",
        "mountains",
        "beach",
        "beaches",
        "forest",
        "woods",
        "lake",
        "river",
        "waterfall",
        "waterfalls",
        "sunset",
        "sunrise",
        "sky",
        "clouds",
        "ocean",
        "sea",
        "city",
        "street",
        "night",
        "landscape",
        "landscapes",
        "park",
        "garden",
        "snow",
        "winter",
        "summer",
        "spring",
        "fall",
        "autumn",
        "cafe",
        "restaurant",
        "kitchen",
        "pool",
        "wedding",
        "concert",
        "canyon",
        "desert",
        "island",
        "trail",
        "coast",
        "shore",
        "hills",
        "valley",
        "field",
        "meadow",
        "backyard",
        "patio",
    ];
    // Relative date & season rescue: run BEFORE moving season words from location_query to semantic_query,
    // and allow combining with person/camera filters (e.g. "summer photos of Sebastian").
    if plan.date_from_ms.is_none() && plan.date_to_ms.is_none() {
        let today = now_utc.date_naive();
        let cur_y = today.year();
        let cur_m = today.month();
        let rescue_range: Option<((i64, i64), &str)> = if has_word(&lower, "today") {
            let s = Utc.from_utc_datetime(&today.and_hms_opt(0, 0, 0).expect("valid"));
            let e =
                Utc.from_utc_datetime(&today.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "today"))
        } else if has_word(&lower, "yesterday") {
            let yday = today - chrono::Duration::days(1);
            let s = Utc.from_utc_datetime(&yday.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&yday.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "yesterday"))
        } else if lower.contains("last weekend") {
            let (sat, sun) = last_weekend_dates(today);
            let s = Utc.from_utc_datetime(&sat.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&sun.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "last weekend"))
        } else if lower.contains("this week") {
            let days_from_mon = today.weekday().num_days_from_monday() as i64;
            let mon = today - chrono::Duration::days(days_from_mon);
            let s = Utc.from_utc_datetime(&mon.and_hms_opt(0, 0, 0).expect("valid"));
            let e =
                Utc.from_utc_datetime(&today.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "this week"))
        } else if lower.contains("last week") {
            let start = today - chrono::Duration::days(7);
            let s = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0).expect("valid"));
            let e =
                Utc.from_utc_datetime(&today.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "last week"))
        } else if lower.contains("last 30 days") || lower.contains("past 30 days") {
            let start = today - chrono::Duration::days(30);
            let s = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0).expect("valid"));
            let e =
                Utc.from_utc_datetime(&today.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "last 30 days"))
        } else if lower.contains("this month") {
            let s_d = NaiveDate::from_ymd_opt(cur_y, cur_m, 1).expect("valid");
            let e_d = NaiveDate::from_ymd_opt(cur_y, cur_m, calendar_month_last_day(cur_y, cur_m))
                .expect("valid");
            let s = Utc.from_utc_datetime(&s_d.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&e_d.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "this month"))
        } else if lower.contains("last month") {
            let (py, pm) = shift_calendar_month(cur_y, cur_m, -1);
            let s_d = NaiveDate::from_ymd_opt(py, pm, 1).expect("valid");
            let e_d =
                NaiveDate::from_ymd_opt(py, pm, calendar_month_last_day(py, pm)).expect("valid");
            let s = Utc.from_utc_datetime(&s_d.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&e_d.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "last month"))
        } else if lower.contains("two months ago") || lower.contains("2 months ago") {
            let (py, pm) = shift_calendar_month(cur_y, cur_m, -2);
            let s_d = NaiveDate::from_ymd_opt(py, pm, 1).expect("valid");
            let e_d =
                NaiveDate::from_ymd_opt(py, pm, calendar_month_last_day(py, pm)).expect("valid");
            let s = Utc.from_utc_datetime(&s_d.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&e_d.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some((
                (s.timestamp_millis(), e.timestamp_millis()),
                "two months ago",
            ))
        } else if lower.contains("this year") {
            let s_d = NaiveDate::from_ymd_opt(cur_y, 1, 1).expect("valid");
            let e_d = NaiveDate::from_ymd_opt(cur_y, 12, 31).expect("valid");
            let s = Utc.from_utc_datetime(&s_d.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&e_d.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "this year"))
        } else if lower.contains("last year") {
            let py = cur_y - 1;
            let s_d = NaiveDate::from_ymd_opt(py, 1, 1).expect("valid");
            let e_d = NaiveDate::from_ymd_opt(py, 12, 31).expect("valid");
            let s = Utc.from_utc_datetime(&s_d.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&e_d.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "last year"))
        } else if has_word(&lower, "christmas") || has_word(&lower, "xmas") {
            let cy = extract_explicit_year(&lower).unwrap_or(
                if today.month() == 12 && today.day() >= 24 {
                    today.year()
                } else {
                    today.year() - 1
                },
            );
            let s_d = NaiveDate::from_ymd_opt(cy, 12, 24).expect("valid");
            let e_d = NaiveDate::from_ymd_opt(cy, 12, 26).expect("valid");
            let s = Utc.from_utc_datetime(&s_d.and_hms_opt(0, 0, 0).expect("valid"));
            let e = Utc.from_utc_datetime(&e_d.and_hms_milli_opt(23, 59, 59, 999).expect("valid"));
            Some(((s.timestamp_millis(), e.timestamp_millis()), "Christmas"))
        } else {
            season_date_range_ms(&lower, now_utc).map(|r| {
                let name = if has_word(&lower, "summer") {
                    "summer"
                } else if has_word(&lower, "spring") {
                    "spring"
                } else if has_word(&lower, "fall") || has_word(&lower, "autumn") {
                    "fall"
                } else {
                    "winter"
                };
                (r, name)
            })
        };

        if let Some(((from_ms, to_ms), label)) = rescue_range {
            plan.date_from_ms = Some(from_ms);
            plan.date_to_ms = Some(to_ms);
            if plan.location_query.as_deref().is_some_and(|l| {
                matches!(
                    l.trim().to_ascii_lowercase().as_str(),
                    "summer"
                        | "spring"
                        | "fall"
                        | "autumn"
                        | "winter"
                        | "yesterday"
                        | "christmas"
                        | "xmas"
                )
            }) {
                plan.location_query = None;
            }
            if plan.semantic_query.as_deref().is_some_and(|s| {
                matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "summer"
                        | "spring"
                        | "fall"
                        | "autumn"
                        | "winter"
                        | "yesterday"
                        | "last weekend"
                )
            }) {
                plan.semantic_query = None;
            }
            if !plan_has_any_criteria(prior) {
                plan.clear_prior_filters = true;
            }
            if plan.assistant_reply.trim().is_empty()
                || plan
                    .assistant_reply
                    .to_ascii_lowercase()
                    .contains("didn't find")
            {
                plan.assistant_reply = format!("Here are your {label} photos.");
            }
        }
    }

    if let Some(loc) = plan.location_query.as_deref() {
        let loc_trimmed = loc.trim().to_ascii_lowercase();
        let loc_low = loc_trimmed
            .strip_prefix("the ")
            .or_else(|| loc_trimmed.strip_prefix("a "))
            .or_else(|| loc_trimmed.strip_prefix("my "))
            .unwrap_or(&loc_trimmed);
        if GENERIC_SCENE_LOCATIONS.contains(&loc_low) {
            let moved = plan.location_query.take();
            if plan.semantic_query.is_none() {
                plan.semantic_query = match loc_low {
                    "outdoor" | "outdoors" => Some("outdoor nature landscape".to_string()),
                    "indoor" | "indoors" => Some("indoor room interior".to_string()),
                    _ => moved.map(|m| {
                        m.trim()
                            .strip_prefix("the ")
                            .or_else(|| m.trim().strip_prefix("The "))
                            .unwrap_or(m.trim())
                            .to_string()
                    }),
                };
            }
        }
    }

    // Strip subjective filler words ("cool", "best", "nice", "stuff", "pics") from semantic_query,
    // and strip "wallpaper"/"wallpapers" when an orientation filter is already active.
    if let Some(sq) = plan.semantic_query.take() {
        let cleaned = strip_semantic_filler(&sq);
        plan.semantic_query = cleaned.and_then(|s| {
            let low = s.to_ascii_lowercase();
            if plan.orientation.is_some()
                && matches!(
                    low.as_str(),
                    "wallpaper"
                        | "wallpapers"
                        | "phone wallpaper"
                        | "phone wallpapers"
                        | "desktop wallpaper"
                        | "desktop wallpapers"
                        | "phone background"
                        | "desktop background"
                        | "lock screen"
                )
            {
                None
            } else {
                Some(s)
            }
        });
    }

    // Broad people request rescue ("show me people" / "show me my friends" with no specific name).
    if !wants_no_people
        && plan.person_names.is_empty()
        && plan.has_people.is_none()
        && (has_word(&lower, "people")
            || has_word(&lower, "friends")
            || has_word(&lower, "everyone")
            || has_word(&lower, "folks"))
        && !lower.contains("friends'")
        && !lower.contains("everyone's")
    {
        plan.has_people = Some(true);
        if let Some(sq) = plan.semantic_query.as_deref() {
            let sq_low = sq.trim().to_ascii_lowercase();
            if matches!(
                sq_low.as_str(),
                "people" | "person" | "friends" | "friend" | "everyone" | "folks" | "portraits"
            ) {
                plan.semantic_query = None;
            }
        }
    }

    // Vague & exploratory browsing rescue: if the plan still has no criteria (or after filler stripping),
    // map recent/latest/everything -> capture_desc, and cool/best/surprise/interesting -> aperture_asc.
    let explicit_reset = [
        "start over",
        "reset",
        "clear filter",
        "clear all",
        "new search",
        "nevermind",
        "never mind",
        "forget that",
        "scratch that",
    ]
    .iter()
    .any(|kw| lower.contains(kw));

    let effective_has_criteria =
        plan_has_any_criteria(plan) || (!plan.clear_prior_filters && plan_has_any_criteria(prior));
    if !effective_has_criteria && !explicit_reset {
        let wants_recent = [
            "recent",
            "latest",
            "newest",
            "what did i shoot",
            "what have i shot",
            "everything",
            "all my",
            "my photos",
            "my pics",
            "browse",
        ]
        .iter()
        .any(|kw| lower.contains(kw));

        if wants_recent {
            plan.sort_by = Some("capture_desc".to_string());
            plan.clear_prior_filters = true;
        } else if wants_exploratory {
            plan.sort_by = Some("best".to_string());
            plan.clear_prior_filters = true;
        }
    }
}

fn calendar_month_last_day(year: i32, month: u32) -> u32 {
    let (next_y, next_m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_y, next_m, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

static SIDECAR_HTTP: OnceLock<Result<(tokio::runtime::Runtime, reqwest::Client), String>> =
    OnceLock::new();

fn sidecar_http_client() -> Result<&'static (tokio::runtime::Runtime, reqwest::Client), AiError> {
    let init = SIDECAR_HTTP.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tp-sidecar-http")
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|e| format!("reqwest client: {e}"))?;
        Ok((rt, client))
    });
    init.as_ref().map_err(|e| AiError::ExternalTool(e.clone()))
}

fn run_blocking_http<F, T>(fut_fn: F) -> Result<T, AiError>
where
    F: FnOnce(
            reqwest::Client,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, AiError>> + Send>>
        + Send
        + 'static,
    T: Send + 'static,
{
    let (rt, client) = sidecar_http_client()?;
    let client_clone = client.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    rt.spawn(async move {
        let res = fut_fn(client_clone).await;
        let _ = tx.send(res);
    });
    rx.recv()
        .map_err(|_| AiError::ExternalTool("sidecar HTTP channel closed".to_string()))?
}

fn wait_for_server_health(
    port: u16,
    child: &mut Child,
    timeout: Duration,
    shutting_down: &AtomicBool,
) -> Result<(), AiError> {
    let deadline = Instant::now() + timeout;
    let url = format!("http://127.0.0.1:{port}/health");
    loop {
        if shutting_down.load(Ordering::Acquire) {
            return Err(AiError::Cancelled);
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| AiError::ExternalTool(format!("llama-server wait: {e}")))?
        {
            return Err(AiError::ExternalTool(format!(
                "llama-server exited early with status {status}"
            )));
        }

        let url_req = url.clone();
        let ok = run_blocking_http(move |client| {
            Box::pin(async move {
                Ok(client
                    .get(&url_req)
                    .timeout(Duration::from_millis(500))
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

fn post_chat_completion(port: u16, payload: &Value, timeout: Duration) -> Result<String, AiError> {
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = payload.clone();

    run_blocking_http(move |client| {
        Box::pin(async move {
            let resp = client
                .post(&url)
                .timeout(timeout)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap()
    }

    #[test]
    fn sanitize_routes_meta_questions_to_answer_and_clears_filters() {
        let mut plan = RouterPlan {
            intent: Some("search".to_string()),
            person_names: vec!["Sebastian".to_string(), "AJ".to_string()],
            has_people: Some(true),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "who are in my pics",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut plan,
        );
        assert_eq!(plan.intent.as_deref(), Some("answer"));
        assert!(plan.person_names.is_empty());
        assert_eq!(plan.has_people, None);
        assert!(plan.clear_prior_filters);

        // Filtered count question ("how many photos of Sebastian") must stay a search!
        let mut count_person = RouterPlan {
            intent: Some("search".to_string()),
            person_names: vec!["Sebastian".to_string()],
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "how many photos of Sebastian do I have?",
            &RouterPlan::default(),
            &[],
            &["Sebastian".to_string()],
            fixed_now(),
            &mut count_person,
        );
        assert_eq!(count_person.intent.as_deref(), Some("search"));
        assert_eq!(count_person.person_names, vec!["Sebastian".to_string()]);
    }

    #[test]
    fn sanitize_strips_hallucinated_has_people_false_and_aperture_sort() {
        let mut plan = RouterPlan {
            intent: Some("search".to_string()),
            semantic_query: Some("animals".to_string()),
            has_people: Some(false),
            sort_by: Some("aperture_asc".to_string()),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "show me animals",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut plan,
        );
        assert_eq!(plan.intent.as_deref(), Some("search"));
        assert_eq!(plan.semantic_query.as_deref(), Some("animals"));
        assert_eq!(plan.has_people, None);
        assert_eq!(plan.sort_by, None);

        // Explicit negation must be preserved.
        let mut no_people = RouterPlan {
            semantic_query: Some("mountain".to_string()),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "mountain photos without people",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut no_people,
        );
        assert_eq!(no_people.has_people, Some(false));
    }

    #[test]
    fn sanitize_strips_filler_and_rescues_vague_browsing() {
        let mut mountain = RouterPlan {
            semantic_query: Some("cool mountain pics".to_string()),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "show me cool mountain pics",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut mountain,
        );
        assert_eq!(mountain.semantic_query.as_deref(), Some("mountain"));

        let mut cool = RouterPlan {
            semantic_query: Some("cool pics".to_string()),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "show me cool pics",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut cool,
        );
        assert_eq!(cool.semantic_query, None);
        assert_eq!(cool.sort_by.as_deref(), Some("best"));

        let mut recent = RouterPlan::default();
        sanitize_sidecar_plan(
            "what did I shoot recently?",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut recent,
        );
        assert_eq!(recent.sort_by.as_deref(), Some("capture_desc"));
    }

    #[test]
    fn sanitize_handles_single_filter_removal_and_speculative_tag_promotion() {
        let prior = RouterPlan {
            intent: Some("search".to_string()),
            person_names: vec!["Sebastian".to_string()],
            location_query: Some("Multnomah Falls".to_string()),
            ..Default::default()
        };
        let mut removed = RouterPlan::default();
        sanitize_sidecar_plan(
            "remove the location filter",
            &prior,
            &[],
            &[],
            fixed_now(),
            &mut removed,
        );
        assert_eq!(removed.location_query, None);
        assert_eq!(removed.person_names, vec!["Sebastian".to_string()]);
        assert!(removed.clear_prior_filters);

        let mut zoo_tag = RouterPlan {
            tags: vec!["zoo".to_string()],
            sort_by: Some("iso_desc".to_string()),
            ..Default::default()
        };
        let folders = vec!["Portland Zoo 6-21 (146)".to_string()];
        sanitize_sidecar_plan(
            "highest ISO zoo photos",
            &RouterPlan::default(),
            &folders,
            &[],
            fixed_now(),
            &mut zoo_tag,
        );
        assert!(zoo_tag.tags.is_empty());
        assert_eq!(zoo_tag.location_query.as_deref(), Some("Portland Zoo 6-21"));

        // Generic terrain misplaced into location_query -> moved to semantic_query.
        let mut outdoors = RouterPlan {
            location_query: Some("outdoor".to_string()),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "show me outdoors",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut outdoors,
        );
        assert_eq!(outdoors.location_query, None);
        assert_eq!(
            outdoors.semantic_query.as_deref(),
            Some("outdoor nature landscape")
        );

        // Season query rescued to date range when LLM left filters empty.
        let mut summer = RouterPlan::default();
        sanitize_sidecar_plan(
            "show me summer photos",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut summer,
        );
        assert!(summer.date_from_ms.is_some());
        assert!(summer.date_to_ms.is_some());
    }

    #[test]
    fn sanitize_all_tier_edge_cases_and_small_model_guards() {
        // 1. "grainy" / "noisy" ISO min must NOT collapse into exact ISO band.
        let mut grainy = RouterPlan {
            iso_min: Some(3200),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "grainy night photos",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut grainy,
        );
        assert_eq!(grainy.iso_min, Some(3200));
        assert_eq!(grainy.iso_max, None);

        // 2. "ultrawide" focal max must NOT collapse into exact focal length.
        let mut ultrawide = RouterPlan {
            focal_length_max_mm: Some(24.0),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "ultrawide architecture shots",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut ultrawide,
        );
        assert_eq!(ultrawide.focal_length_min_mm, None);
        assert_eq!(ultrawide.focal_length_max_mm, Some(24.0));

        // 3. Long exposure "20s" must NOT be stripped as a decade!
        let mut twenty_s = RouterPlan {
            shutter_speed_min_sec: Some(20.0),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "20s star trails",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut twenty_s,
        );
        assert_eq!(twenty_s.shutter_speed_min_sec, Some(20.0));

        // 4. Orientation cues: panorama & phone wallpaper.
        let mut pano = RouterPlan::default();
        sanitize_sidecar_plan(
            "show me panoramas",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut pano,
        );
        assert_eq!(pano.orientation.as_deref(), Some("panorama"));

        // 5. Tier 1 hallucinated kind=Photo and favorite=false stripped on casual queries.
        let mut hallucinated = RouterPlan {
            semantic_query: Some("dogs".to_string()),
            kind: Some(AssetKind::Photo),
            favorite: Some(false),
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "photos of dogs",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut hallucinated,
        );
        assert_eq!(hallucinated.kind, None);
        assert_eq!(hallucinated.favorite, None);

        // 6. Face-count rescue: "solo photo of Sebastian" -> max_faces = 1.
        let mut solo = RouterPlan {
            person_names: vec!["Sebastian".to_string()],
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "solo photo of Sebastian",
            &RouterPlan::default(),
            &[],
            &["Sebastian".to_string()],
            fixed_now(),
            &mut solo,
        );
        assert_eq!(solo.max_faces, Some(1));

        // 7. Entity swap ("what about AJ?") replaces prior person_names cleanly.
        let prior_seb = RouterPlan {
            person_names: vec!["Sebastian".to_string()],
            location_query: Some("Portland Zoo".to_string()),
            ..Default::default()
        };
        let mut swap = RouterPlan {
            person_names: vec!["AJ".to_string()],
            ..Default::default()
        };
        sanitize_sidecar_plan(
            "what about AJ?",
            &prior_seb,
            &[],
            &["Sebastian".to_string(), "AJ".to_string()],
            fixed_now(),
            &mut swap,
        );
        assert!(swap.clear_prior_filters);
        assert_eq!(swap.person_names, vec!["AJ".to_string()]);
        assert_eq!(swap.location_query.as_deref(), Some("Portland Zoo"));

        // 8. Proper-noun preservation & b&w expansion in strip_semantic_filler.
        assert_eq!(
            strip_semantic_filler("Best Buy").as_deref(),
            Some("Best Buy")
        );
        assert_eq!(
            strip_semantic_filler("b&w").as_deref(),
            Some("black and white monochrome")
        );

        // 9. Explicit year on season: "summer 2025".
        let mut s25 = RouterPlan::default();
        sanitize_sidecar_plan(
            "summer 2025",
            &RouterPlan::default(),
            &[],
            &[],
            fixed_now(),
            &mut s25,
        );
        assert_eq!(
            s25.date_from_ms,
            Some(
                Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
    }
}
