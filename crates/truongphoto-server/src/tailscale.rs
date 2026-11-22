use crate::error::AppError;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use truongphoto_core::TailscaleStatus;

const CLI_TIMEOUT: Duration = Duration::from_secs(15);
const STATUS_CACHE_TTL: Duration = Duration::from_secs(30);

/// Captured stdout/stderr and exit status from running the Tailscale CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleCmdOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Mockable abstraction over the local `tailscale` CLI executable.
pub trait TailscaleRunner: Send + Sync {
    fn discover_cli(&self) -> Option<PathBuf>;

    fn run<'a>(
        &'a self,
        cli: &'a Path,
        args: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = Result<TailscaleCmdOutput, String>> + Send + 'a>>;
}

/// Production `TailscaleRunner` invoking the OS Tailscale CLI binary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemTailscaleRunner;

impl TailscaleRunner for SystemTailscaleRunner {
    fn discover_cli(&self) -> Option<PathBuf> {
        if let Ok(override_bin) = std::env::var("TP_TAILSCALE_BIN") {
            let trimmed = override_bin.trim();
            if !trimmed.is_empty() {
                let candidate = PathBuf::from(trimmed);
                if candidate.is_file() {
                    return Some(candidate);
                }
                if let Some(found) = find_in_path(trimmed) {
                    return Some(found);
                }
            }
        }

        let macos_standalone = Path::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        if macos_standalone.is_file() {
            return Some(macos_standalone.to_path_buf());
        }

        for windows_cand in [
            r"C:\Program Files\Tailscale\tailscale.exe",
            r"C:\Program Files\Tailscale IPN\tailscale.exe",
        ] {
            let p = Path::new(windows_cand);
            if p.is_file() {
                return Some(p.to_path_buf());
            }
        }

        for bin in ["tailscale", "tailscale.exe"] {
            if let Some(found) = find_in_path(bin) {
                return Some(found);
            }
        }

        None
    }

    fn run<'a>(
        &'a self,
        cli: &'a Path,
        args: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = Result<TailscaleCmdOutput, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(cli);
            #[cfg(windows)]
            {
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let fut = cmd.args(args).output();
            let output = tokio::time::timeout(CLI_TIMEOUT, fut)
                .await
                .map_err(|_| format!("Tailscale CLI timed out after {}s", CLI_TIMEOUT.as_secs()))?
                .map_err(|e| format!("Failed to execute Tailscale CLI: {e}"))?;

            Ok(TailscaleCmdOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Coordinates Tailscale status preflight, `tailscale serve --bg`, and actionable error mapping.
pub struct TailscaleManager {
    runner: Arc<dyn TailscaleRunner>,
    last_error: Mutex<Option<String>>,
    cached_status: RwLock<Option<(Instant, TailscaleStatus)>>,
}

impl TailscaleManager {
    pub fn new(runner: Arc<dyn TailscaleRunner>) -> Self {
        Self {
            runner,
            last_error: Mutex::new(None),
            cached_status: RwLock::new(None),
        }
    }

    fn set_last_error(&self, err: Option<String>) {
        *self.last_error.lock() = err;
    }

    fn current_last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    fn cache_status(&self, status: &TailscaleStatus) {
        *self.cached_status.write() = Some((Instant::now(), status.clone()));
    }

    /// Returns a recently cached `TailscaleStatus` (within 30s) or refreshes it.
    pub async fn cached_or_refresh_status(&self, bound_port: u16) -> TailscaleStatus {
        {
            let guard = self.cached_status.read();
            if let Some((captured_at, status)) = guard.as_ref() {
                if captured_at.elapsed() < STATUS_CACHE_TTL {
                    return status.clone();
                }
            }
        }
        self.get_status(bound_port).await
    }

    /// Probes the Tailscale CLI, daemon state, MagicDNS name, HTTPS certs, and `serve` rules.
    pub async fn get_status(&self, bound_port: u16) -> TailscaleStatus {
        let Some(cli_path) = self.runner.discover_cli() else {
            let status = TailscaleStatus {
                installed: false,
                running: false,
                logged_in: false,
                cli_path: None,
                magic_dns_name: None,
                https_certs_enabled: false,
                serve_active: false,
                https_url: None,
                remediation: Some(
                    "Install Tailscale (Standalone macOS pkg or Windows installer) to enable encrypted remote access."
                        .to_string(),
                ),
                last_error: self.current_last_error(),
            };
            self.cache_status(&status);
            return status;
        };

        let cli_str = cli_path.to_string_lossy().into_owned();
        let status_out = match self.runner.run(&cli_path, &["status", "--json"]).await {
            Ok(out) => out,
            Err(err) => {
                self.set_last_error(Some(err.clone()));
                let status = TailscaleStatus {
                    installed: true,
                    running: false,
                    logged_in: false,
                    cli_path: Some(cli_str),
                    magic_dns_name: None,
                    https_certs_enabled: false,
                    serve_active: false,
                    https_url: None,
                    remediation: Some(
                        "Start Tailscale and sign in on this machine, then retry.".to_string(),
                    ),
                    last_error: Some(err),
                };
                self.cache_status(&status);
                return status;
            }
        };

        let parsed = serde_json::from_str::<Value>(&status_out.stdout).ok();
        let backend_state = parsed
            .as_ref()
            .and_then(|v| v.get("BackendState"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        let (running, logged_in) = match backend_state {
            "Running" => (true, true),
            "NeedsLogin" | "NeedsMachineAuth" => (true, false),
            _ => (false, false),
        };

        let magic_dns_name = parsed
            .as_ref()
            .and_then(|v| v.get("Self"))
            .and_then(|s| s.get("DNSName"))
            .and_then(Value::as_str)
            .map(|s| s.trim().trim_end_matches('.').to_string())
            .filter(|s| !s.is_empty());

        let https_certs_enabled = parsed
            .as_ref()
            .and_then(|v| v.get("CertDomains"))
            .and_then(Value::as_array)
            .is_some_and(|arr| !arr.is_empty());

        let mut last_err = self.current_last_error();
        if (!status_out.success && parsed.is_none())
            || (!running && !status_out.stderr.trim().is_empty())
        {
            let detail = non_empty_cmd_error(&status_out);
            if !detail.is_empty() {
                last_err = Some(detail);
            }
        }

        let mut serve_active = false;
        let mut https_url = None;

        if running && logged_in {
            match self
                .runner
                .run(&cli_path, &["serve", "status", "--json"])
                .await
            {
                Ok(serve_out) if serve_out.success => {
                    if let Ok(serve_json) = serde_json::from_str::<Value>(&serve_out.stdout) {
                        if serve_targets_port(&serve_json, bound_port) {
                            serve_active = true;
                            if let Some(ref dns) = magic_dns_name {
                                https_url = Some(format!("https://{dns}"));
                            }
                        }
                    }
                }
                Ok(serve_out) => {
                    let detail = non_empty_cmd_error(&serve_out);
                    if !detail.is_empty() {
                        last_err = Some(detail);
                    }
                }
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }

        let remediation = if !running || !logged_in {
            Some("Sign in to Tailscale on this machine, then retry.".to_string())
        } else if !https_certs_enabled {
            Some(
                "Enable MagicDNS and HTTPS Certificates in your Tailscale Admin Console -> DNS."
                    .to_string(),
            )
        } else {
            None
        };

        let status = TailscaleStatus {
            installed: true,
            running,
            logged_in,
            cli_path: Some(cli_str),
            magic_dns_name,
            https_certs_enabled,
            serve_active,
            https_url,
            remediation,
            last_error: last_err,
        };
        self.cache_status(&status);
        status
    }

    /// Runs preflight checks and enables `tailscale serve --bg https / http://127.0.0.1:<port>`.
    pub async fn enable(&self, bound_port: u16) -> Result<TailscaleStatus, AppError> {
        let preflight = self.get_status(bound_port).await;
        if !preflight.installed {
            return Err(AppError::conflict(preflight.remediation.unwrap_or_else(
                || "Install Tailscale before enabling remote HTTPS.".to_string(),
            )));
        }
        if !preflight.running || !preflight.logged_in {
            return Err(AppError::conflict(preflight.remediation.unwrap_or_else(
                || "Sign in to Tailscale on this machine, then retry.".to_string(),
            )));
        }
        if !preflight.https_certs_enabled {
            return Err(AppError::conflict(preflight.remediation.unwrap_or_else(
                || {
                    "Enable MagicDNS and HTTPS Certificates in your Tailscale Admin Console -> DNS."
                        .to_string()
                },
            )));
        }

        let cli_path = PathBuf::from(
            preflight
                .cli_path
                .as_deref()
                .ok_or_else(|| AppError::conflict("Tailscale CLI path not found."))?,
        );
        let target = format!("http://127.0.0.1:{bound_port}");
        let output = self
            .runner
            .run(&cli_path, &["serve", "--bg", "https", "/", &target])
            .await
            .map_err(|e| {
                self.set_last_error(Some(e.clone()));
                AppError::internal(e)
            })?;

        if !output.success {
            let err = self.map_command_failure("enable Tailscale Serve", &output);
            return Err(err);
        }

        self.set_last_error(None);
        Ok(self.get_status(bound_port).await)
    }

    /// Disables Tailscale Serve (`tailscale serve --https=443 off`, falling back to `tailscale serve reset`).
    pub async fn disable(&self, bound_port: u16) -> Result<TailscaleStatus, AppError> {
        let Some(cli_path) = self.runner.discover_cli() else {
            return Err(AppError::conflict(
                "Install Tailscale before managing remote HTTPS.".to_string(),
            ));
        };

        let first = self
            .runner
            .run(&cli_path, &["serve", "--https=443", "off"])
            .await
            .map_err(|e| {
                self.set_last_error(Some(e.clone()));
                AppError::internal(e)
            })?;

        if !first.success {
            let fallback = self
                .runner
                .run(&cli_path, &["serve", "reset"])
                .await
                .map_err(|e| {
                    self.set_last_error(Some(e.clone()));
                    AppError::internal(e)
                })?;

            if !fallback.success {
                return Err(self.map_command_failure("disable Tailscale Serve", &fallback));
            }
        }

        self.set_last_error(None);
        Ok(self.get_status(bound_port).await)
    }

    fn map_command_failure(&self, action: &str, output: &TailscaleCmdOutput) -> AppError {
        let detail = non_empty_cmd_error(output);
        let lower = detail.to_ascii_lowercase();
        self.set_last_error(Some(detail.clone()));

        if lower.contains("encryptstate") || lower.contains("operation not permitted") {
            return AppError::conflict(
                "Mac App Store Tailscale sandbox blocks `tailscale serve` (EncryptState / Operation not permitted). Install the Standalone macOS Tailscale package from tailscale.com/download and retry.",
            );
        }

        if lower.contains("access is denied") || lower.contains("requires elevation") {
            return AppError::forbidden(
                "Tailscale requires Windows Administrator elevation to manage `tailscale serve`. Run TruongPhoto as Administrator or run `tailscale set --operator=$env:USERNAME` in an elevated PowerShell prompt.",
            );
        }

        if detail.is_empty() {
            AppError::internal(format!("Failed to {action}."))
        } else {
            AppError::internal(format!("Failed to {action}: {detail}"))
        }
    }
}

fn non_empty_cmd_error(output: &TailscaleCmdOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    output.stdout.trim().to_string()
}

fn serve_targets_port(value: &Value, bound_port: u16) -> bool {
    match value {
        Value::String(s) => proxy_targets_port(s, bound_port),
        Value::Array(arr) => arr.iter().any(|v| serve_targets_port(v, bound_port)),
        Value::Object(map) => map.values().any(|v| serve_targets_port(v, bound_port)),
        _ => false,
    }
}

fn proxy_targets_port(proxy: &str, bound_port: u16) -> bool {
    let loopback = format!("127.0.0.1:{bound_port}");
    let localhost = format!("localhost:{bound_port}");
    for needle in [&loopback, &localhost] {
        if let Some(pos) = proxy.find(needle.as_str()) {
            let rest = &proxy[pos + needle.len()..];
            if rest.is_empty() || rest.starts_with('/') {
                return true;
            }
        }
    }
    false
}
