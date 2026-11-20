use crate::ai::AiCoordinator;
use crate::ingest::{
    cache_mgr::CacheManager, scanner::ScanCoordinator, watcher::FolderWatcherManager,
};
use crate::mdns::MdnsHandle;
use crate::rate_limit::AuthRateLimiter;
use crate::tailscale::{SystemTailscaleRunner, TailscaleManager, TailscaleRunner};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Instant;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use truongphoto_ai::AiPipeline;
use truongphoto_core::{
    HardwareInfo, PowerState, ServerConfig, StoragePaths, ThrottleStatus, WsServerEvent,
};
use truongphoto_db::DbHandle;

/// Shared application state injected into every Axum route and extractor.
pub struct AppState {
    pub db: DbHandle,
    pub config: ServerConfig,
    pub default_paths: StoragePaths,
    pub started_at: Instant,
    pub rate_limiter: AuthRateLimiter,
    pub gate_key: [u8; 32],
    pub desktop_secret: Option<String>,
    pub ws_tx: broadcast::Sender<WsServerEvent>,
    pub power_rx: watch::Receiver<PowerState>,
    pub throttle_tx: watch::Sender<ThrottleStatus>,
    pub throttle_rx: watch::Receiver<ThrottleStatus>,
    pub mdns: Mutex<MdnsHandle>,
    pub hardware: HardwareInfo,
    pub bound_port: u16,
    pub shutdown_token: CancellationToken,
    pub cache_mgr: Arc<CacheManager>,
    pub scanner: Arc<ScanCoordinator>,
    pub watcher: Arc<FolderWatcherManager>,
    pub ai: Arc<AiCoordinator>,
    pub tailscale: Arc<TailscaleManager>,
    sys: Mutex<System>,
    pid: Pid,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: DbHandle,
        config: ServerConfig,
        gate_key: [u8; 32],
        ws_tx: broadcast::Sender<WsServerEvent>,
        power_rx: watch::Receiver<PowerState>,
        mdns: MdnsHandle,
        hardware: HardwareInfo,
        bound_port: u16,
        shutdown_token: CancellationToken,
    ) -> Self {
        let default_paths = StoragePaths::from_data_dir(config.data_dir.clone());
        let desktop_secret = config.desktop_bootstrap_token.clone();
        let pid = Pid::from_u32(std::process::id());
        let sys = System::new();
        let cache_mgr = Arc::new(CacheManager::new(db.clone()));
        let scanner = Arc::new(ScanCoordinator::new());
        let watcher = Arc::new(FolderWatcherManager::new());
        let ai = Arc::new(AiCoordinator::production(&default_paths));
        let tailscale = Arc::new(TailscaleManager::new(Arc::new(SystemTailscaleRunner)));
        let (throttle_tx, throttle_rx) = watch::channel(ThrottleStatus::default());

        Self {
            db,
            config,
            default_paths,
            started_at: Instant::now(),
            rate_limiter: AuthRateLimiter::new(),
            gate_key,
            desktop_secret,
            ws_tx,
            power_rx,
            throttle_tx,
            throttle_rx,
            mdns: Mutex::new(mdns),
            hardware,
            bound_port,
            shutdown_token,
            cache_mgr,
            scanner,
            watcher,
            ai,
            tailscale,
            sys: Mutex::new(sys),
            pid,
        }
    }

    /// Replaces the AI pipeline (e.g. injecting `AiPipeline::deterministic_test()` for offline tests).
    pub fn with_ai_pipeline(mut self, pipeline: AiPipeline) -> Self {
        let vectors_dir = self.default_paths.data_dir.join("vectors");
        let models_dir = self.default_paths.data_dir.join("models");
        self.ai = Arc::new(AiCoordinator::new(
            vectors_dir,
            models_dir,
            Arc::new(pipeline),
        ));
        self
    }

    /// Replaces the Tailscale CLI runner (e.g. injecting a mock `TailscaleRunner` in tests).
    pub fn with_tailscale_runner(mut self, runner: Arc<dyn TailscaleRunner>) -> Self {
        self.tailscale = Arc::new(TailscaleManager::new(runner));
        self
    }

    /// Returns the resident memory usage of the current process in bytes.
    pub fn process_ram_bytes(&self) -> u64 {
        let mut sys = self.sys.lock();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        sys.process(self.pid).map(|p| p.memory()).unwrap_or(0)
    }
}
