use crate::AiError;
use ort::environment::Environment;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    OcrRec,
}

struct CachedSession {
    session: Session,
    last_used: Instant,
}

struct GlobalOrtState {
    env: Environment,
    dylib_path: PathBuf,
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
    let Some(dylib_path) = discover_ort_dylib() else {
        return Err("ONNX Runtime dynamic library not found".to_string());
    };

    let env = ort::init_from(&dylib_path)
        .map_err(|e| format!("ort::init_from({}): {e}", dylib_path.display()))?
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
        path = %dylib_path.display(),
        ep = ep_name,
        "initialized ONNX Runtime dynamic library"
    );

    Ok(GlobalOrtState {
        env,
        dylib_path,
        ep_name,
    })
}

/// Manages the dynamic ONNX Runtime environment and a lazy per-model session cache with idle eviction.
pub struct OrtRuntimeManager {
    coreml_cache_dir: PathBuf,
    sessions: Mutex<HashMap<SessionKey, Arc<Mutex<Option<CachedSession>>>>>,
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
        }
    }

    /// Returns `true` if ONNX Runtime was successfully loaded dynamically.
    pub fn ort_available(&self) -> bool {
        GLOBAL_ORT.get().is_some_and(Result::is_ok)
    }

    /// Returns `"CoreML"`, `"DirectML"`, `"CPU"`, or `"Unavailable"`.
    pub fn ep_name(&self) -> &'static str {
        match GLOBAL_ORT.get() {
            Some(Ok(state)) => state.ep_name,
            _ => "Unavailable",
        }
    }

    /// Returns the discovered ONNX Runtime dynamic library path, if loaded.
    pub fn dylib_path(&self) -> Option<&Path> {
        match GLOBAL_ORT.get() {
            Some(Ok(state)) => Some(state.dylib_path.as_path()),
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

    /// Immediately drops all cached ONNX Runtime sessions.
    pub fn evict_all_sessions(&self) -> usize {
        self.evict_idle_sessions(Duration::ZERO)
    }

    /// Lazily loads (or reuses) the ONNX Runtime session for `key` and runs `f(&mut session)`.
    pub fn with_session<T, F>(&self, key: SessionKey, model_path: &Path, f: F) -> Result<T, AiError>
    where
        F: FnOnce(&mut Session) -> Result<T, AiError>,
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

        let slot = {
            let mut map = self.sessions.lock();
            map.entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };

        let mut guard = slot.lock();
        if guard.is_none() {
            let session = self.build_session(&state.env, model_path)?;
            *guard = Some(CachedSession {
                session,
                last_used: Instant::now(),
            });
        }

        let cached = guard
            .as_mut()
            .expect("session slot populated immediately above");
        cached.last_used = Instant::now();
        let out = f(&mut cached.session)?;
        cached.last_used = Instant::now();
        Ok(out)
    }

    fn build_session(&self, env: &Environment, model_path: &Path) -> Result<Session, AiError> {
        let _ = std::fs::create_dir_all(&self.coreml_cache_dir);
        let mut builder = Session::builder(env)
            .map_err(|e| AiError::Ort(e.to_string()))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| AiError::Ort(e.to_string()))?;

        #[cfg(target_os = "macos")]
        {
            let cache_dir = self.coreml_cache_dir.to_string_lossy().into_owned();
            let ep = ort::ep::CoreML::default()
                .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
                .with_static_input_shapes(true)
                .with_model_cache_dir(&cache_dir)
                .build();
            builder = builder
                .with_execution_providers([ep])
                .unwrap_or_else(|e| e.recover());
        }

        #[cfg(target_os = "windows")]
        {
            let ep = ort::ep::DirectML::default().build();
            builder = builder
                .with_execution_providers([ep])
                .unwrap_or_else(|e| e.recover());
        }

        builder
            .commit_from_file(model_path)
            .map_err(|e| AiError::Ort(format!("load {}: {e}", model_path.display())))
    }
}
