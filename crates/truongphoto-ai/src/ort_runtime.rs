use crate::AiError;
use ort::environment::Environment;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use truongphoto_core::AiTier;

/// Cache key identifying a lazy-loaded ONNX Runtime inference session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKey {
    VisionImage(AiTier),
    VisionText(AiTier),
    FaceDet,
    FaceRec,
    OcrDet,
    /// Keyed by padded width bucket (`320`, `640`, `960`) so CoreML static-shape
    /// compilation does not reject dynamic width changes across OCR crops.
    OcrRec(u32),
}

struct CachedSession {
    session: Session,
    last_used: Instant,
    is_cpu: bool,
}

struct GlobalOrtState {
    env: Environment,
    dylib_path: Option<PathBuf>,
    ep_name: &'static str,
}

static GLOBAL_ORT: OnceLock<Result<GlobalOrtState, String>> = OnceLock::new();

/// Discovers an ONNX Runtime dynamic library in priority order:
/// 1. `TP_ORT_DYLIB_PATH`
/// 2. Executable sibling / macOS `.app` bundle `Resources`
/// 3. Homebrew / OS standard library locations
pub fn discover_ort_dylib() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("TP_ORT_DYLIB_PATH") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    let lib_filename = if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(lib_filename);
            if sibling.is_file() {
                return Some(sibling);
            }
            if cfg!(target_os = "macos") {
                if let Some(contents) = dir.parent() {
                    let res = contents.join("Resources").join(lib_filename);
                    if res.is_file() {
                        return Some(res);
                    }
                }
            }
        }
    }

    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/opt/homebrew/lib/libonnxruntime.dylib",
            "/usr/local/lib/libonnxruntime.dylib",
        ]
    } else if cfg!(target_os = "windows") {
        &["onnxruntime.dll", r"C:\onnxruntime\lib\onnxruntime.dll"]
    } else {
        &[
            "/usr/local/lib/libonnxruntime.so",
            "/usr/lib/libonnxruntime.so",
        ]
    };

    for candidate in candidates {
        let p = Path::new(candidate);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }

    None
}

fn init_global_ort() -> Result<GlobalOrtState, String> {
    let env = ort::init()
        .with_name("truongphoto")
        .build()
        .map_err(|e| format!("ort environment build: {e}"))?;

    let ep_name = if cfg!(target_os = "macos") {
        "CoreML"
    } else if cfg!(target_os = "windows") {
        "DirectML"
    } else {
        "CPU"
    };

    tracing::info!(
        ep = ep_name,
        "initialized embedded ONNX Runtime (statically linked)"
    );

    Ok(GlobalOrtState {
        env,
        dylib_path: None,
        ep_name,
    })
}

/// Manages the dynamic ONNX Runtime environment and a lazy per-model session cache with idle eviction.
pub struct OrtRuntimeManager {
    coreml_cache_dir: PathBuf,
    sessions: Mutex<HashMap<SessionKey, Arc<Mutex<Option<CachedSession>>>>>,
    /// Session keys that failed hardware EP build or runtime execution and are latched to CPU EP.
    cpu_fallback_keys: Mutex<HashSet<SessionKey>>,
    hw_ep_succeeded: AtomicBool,
    hw_ep_failed: AtomicBool,
}

impl std::fmt::Debug for OrtRuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrtRuntimeManager")
            .field("ort_available", &self.ort_available())
            .field("ep_name", &self.ep_name())
            .field("coreml_cache_dir", &self.coreml_cache_dir)
            .finish()
    }
}

impl OrtRuntimeManager {
    /// Creates an `OrtRuntimeManager` that lazily initializes ONNX Runtime if available.
    /// Never panics if the dynamic library is absent.
    pub fn new(coreml_cache_dir: impl Into<PathBuf>) -> Self {
        let _ = GLOBAL_ORT.get_or_init(init_global_ort);
        Self {
            coreml_cache_dir: coreml_cache_dir.into(),
            sessions: Mutex::new(HashMap::new()),
            cpu_fallback_keys: Mutex::new(HashSet::new()),
            hw_ep_succeeded: AtomicBool::new(false),
            hw_ep_failed: AtomicBool::new(false),
        }
    }

    /// Returns `true` if ONNX Runtime was successfully loaded dynamically.
    pub fn ort_available(&self) -> bool {
        GLOBAL_ORT.get().is_some_and(Result::is_ok)
    }

    /// Returns `"CoreML"`, `"DirectML"`, `"CPU"`, or `"Unavailable"`.
    pub fn ep_name(&self) -> &'static str {
        match GLOBAL_ORT.get() {
            Some(Ok(state)) => {
                if self.hw_ep_failed.load(Ordering::Acquire)
                    && !self.hw_ep_succeeded.load(Ordering::Acquire)
                {
                    "CPU"
                } else {
                    state.ep_name
                }
            }
            _ => "Unavailable",
        }
    }

    /// Returns the discovered ONNX Runtime dynamic library path, if dynamically loaded.
    pub fn dylib_path(&self) -> Option<&Path> {
        match GLOBAL_ORT.get() {
            Some(Ok(state)) => state.dylib_path.as_deref(),
            _ => None,
        }
    }

    /// Returns the number of currently cached live ORT sessions.
    pub fn cached_session_count(&self) -> usize {
        let map = self.sessions.lock();
        map.values().filter(|slot| slot.lock().is_some()).count()
    }

    /// Evicts any cached ONNX Runtime session idle for at least `max_idle`.
    pub fn evict_idle_sessions(&self, max_idle: Duration) -> usize {
        let slots: Vec<Arc<Mutex<Option<CachedSession>>>> =
            self.sessions.lock().values().cloned().collect();
        let now = Instant::now();
        let mut evicted = 0usize;
        for slot in slots {
            let mut guard = slot.lock();
            if let Some(cached) = guard.as_ref() {
                if now.saturating_duration_since(cached.last_used) >= max_idle {
                    *guard = None;
                    evicted += 1;
                }
            }
        }
        evicted
    }

    /// Immediately drops a specific cached ONNX Runtime session to free RAM.
    pub fn evict_session(&self, key: SessionKey) -> bool {
        let slot = self.sessions.lock().get(&key).cloned();
        if let Some(slot) = slot {
            let mut guard = slot.lock();
            if guard.take().is_some() {
                return true;
            }
        }
        false
    }

    /// Immediately drops all cached ONNX Runtime sessions.
    pub fn evict_all_sessions(&self) -> usize {
        self.evict_idle_sessions(Duration::ZERO)
    }

    /// Lazily loads (or reuses) the ONNX Runtime session for `key` and runs `f(&mut session)`.
    /// If a hardware-EP session (DirectML/CoreML) fails at runtime with an ORT error, it is
    /// automatically evicted, latched to CPU EP for `key`, and retried once transparently.
    pub fn with_session<T, F>(
        &self,
        key: SessionKey,
        model_path: &Path,
        mut f: F,
    ) -> Result<T, AiError>
    where
        F: FnMut(&mut Session) -> Result<T, AiError>,
    {
        let state = match GLOBAL_ORT.get_or_init(init_global_ort) {
            Ok(s) => s,
            Err(reason) => return Err(AiError::Ort(reason.clone())),
        };

        if !model_path.is_file() {
            return Err(AiError::ModelNotInstalled(format!(
                "ONNX model file missing: {}",
                model_path.display()
            )));
        }

        // CoreML needs per-width-bucket static sessions for OCR recognition, whereas DirectML/CPU
        // handle dynamic widths cleanly in a single session (saving 3× VRAM on Windows).
        let effective_key = if !cfg!(target_os = "macos") && matches!(key, SessionKey::OcrRec(_)) {
            SessionKey::OcrRec(0)
        } else {
            key
        };

        let slot = {
            let mut map = self.sessions.lock();
            map.entry(effective_key)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };

        let mut guard = slot.lock();
        if guard.is_none() {
            let force_cpu = self.cpu_fallback_keys.lock().contains(&effective_key);
            let (session, is_cpu) =
                self.build_session(&state.env, effective_key, model_path, force_cpu)?;
            *guard = Some(CachedSession {
                session,
                last_used: Instant::now(),
                is_cpu,
            });
        }

        let cached = guard
            .as_mut()
            .expect("session slot populated immediately above");
        cached.last_used = Instant::now();
        let was_cpu = cached.is_cpu;
        match f(&mut cached.session) {
            Ok(out) => {
                cached.last_used = Instant::now();
                Ok(out)
            }
            Err(AiError::Ort(hw_run_err)) if !was_cpu => {
                tracing::warn!(
                    key = ?effective_key,
                    error = %hw_run_err,
                    "hardware EP inference failed at runtime; latching session to CPU EP and retrying"
                );
                self.hw_ep_failed.store(true, Ordering::Release);
                self.cpu_fallback_keys.lock().insert(effective_key);
                *guard = None;
                let (cpu_session, _) =
                    self.build_session(&state.env, effective_key, model_path, true)?;
                *guard = Some(CachedSession {
                    session: cpu_session,
                    last_used: Instant::now(),
                    is_cpu: true,
                });
                let retry_cached = guard.as_mut().expect("cpu session populated above");
                let out = f(&mut retry_cached.session)?;
                retry_cached.last_used = Instant::now();
                Ok(out)
            }
            Err(other) => Err(other),
        }
    }

    fn build_session(
        &self,
        env: &Environment,
        key: SessionKey,
        model_path: &Path,
        force_cpu: bool,
    ) -> Result<(Session, bool), AiError> {
        let _ = key;
        let _ = std::fs::create_dir_all(&self.coreml_cache_dir);
        let intra_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);

        // 1. Try hardware-accelerated execution provider first (CoreML on macOS, DirectML on Windows).
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if !force_cpu {
            let mut hw_builder = Session::builder(env)
                .map_err(|e| AiError::Ort(e.to_string()))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| AiError::Ort(e.to_string()))?
                .with_intra_threads(intra_threads)
                .map_err(|e| AiError::Ort(e.to_string()))?
                .with_inter_threads(1)
                .map_err(|e| AiError::Ort(e.to_string()))?;

            let mut ep_registered = true;

            #[cfg(target_os = "macos")]
            {
                let cache_dir = self.coreml_cache_dir.to_string_lossy().into_owned();
                // VisionText runs with variable batch sizes (8 for tag chunks, tail chunk, and 1 for search queries),
                // so forcing static input shapes causes CoreML compilation or runtime failures on batch changes.
                let use_static_shapes = !matches!(key, SessionKey::VisionText(_));
                let ep = ort::ep::CoreML::default()
                    .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
                    .with_static_input_shapes(use_static_shapes)
                    .with_model_cache_dir(&cache_dir)
                    .build();
                hw_builder = match hw_builder.with_execution_providers([ep]) {
                    Ok(b) => b,
                    Err(e) => {
                        ep_registered = false;
                        e.recover()
                    }
                };
            }

            #[cfg(target_os = "windows")]
            {
                let ep = ort::ep::DirectML::default().build();
                hw_builder = hw_builder
                    .with_memory_pattern(false)
                    .map_err(|e| AiError::Ort(e.to_string()))?
                    .with_parallel_execution(false)
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                hw_builder = match hw_builder.with_execution_providers([ep]) {
                    Ok(b) => b,
                    Err(e) => {
                        ep_registered = false;
                        tracing::warn!(
                            error = %e,
                            "DirectML execution provider registration failed; falling back to CPU EP"
                        );
                        e.recover()
                    }
                };
            }

            if ep_registered {
                match hw_builder.commit_from_file(model_path) {
                    Ok(sess) => {
                        self.hw_ep_succeeded.store(true, Ordering::Release);
                        return Ok((sess, false));
                    }
                    Err(hw_err) => {
                        self.hw_ep_failed.store(true, Ordering::Release);
                        self.cpu_fallback_keys.lock().insert(key);
                        tracing::warn!(
                            model = %model_path.display(),
                            error = %hw_err,
                            "hardware EP session build failed; falling back to CPU EP"
                        );
                    }
                }
            } else {
                self.hw_ep_failed.store(true, Ordering::Release);
                self.cpu_fallback_keys.lock().insert(key);
            }
        }

        let _ = force_cpu;
        // 2. Universal CPU execution provider fallback (100% operator coverage).
        let mut cpu_builder = Session::builder(env)
            .map_err(|e| AiError::Ort(e.to_string()))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| AiError::Ort(e.to_string()))?
            .with_intra_threads(intra_threads)
            .map_err(|e| AiError::Ort(e.to_string()))?
            .with_inter_threads(1)
            .map_err(|e| AiError::Ort(e.to_string()))?;

        let sess = cpu_builder
            .commit_from_file(model_path)
            .map_err(|e| AiError::Ort(format!("load {}: {e}", model_path.display())))?;
        Ok((sess, true))
    }
}
