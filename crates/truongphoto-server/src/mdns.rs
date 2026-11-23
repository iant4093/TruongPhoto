use mdns_sd::{DaemonStatus, ServiceDaemon, ServiceInfo};
use std::time::Duration;

const SERVICE_TYPE: &str = "_http._tcp.local.";
const INSTANCE_NAME: &str = "TruongPhoto";

/// Manages the optional `_http._tcp.local.` Bonjour / mDNS advertisement.
pub struct MdnsHandle {
    daemon: Option<ServiceDaemon>,
    fullname: Option<String>,
    active: bool,
    enabled: bool,
    hostname: String,
    port: u16,
}

impl MdnsHandle {
    /// Initializes mDNS, registering immediately if `enabled` is true.
    ///
    /// Multicast bind failures are logged as warnings and leave `active = false`
    /// without failing server startup.
    pub fn start(hostname: String, port: u16, enabled: bool) -> Self {
        let mut handle = Self {
            daemon: None,
            fullname: None,
            active: false,
            enabled,
            hostname: normalize_hostname(&hostname),
            port,
        };
        if enabled {
            handle.register_service();
        }
        handle
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn hostname(&self) -> &str {
        self.hostname.trim_end_matches('.')
    }

    pub fn mdns_url(&self) -> Option<String> {
        if self.enabled {
            Some(format!("http://{}:{}", self.hostname(), self.port))
        } else {
            None
        }
    }

    /// Toggles mDNS advertisement on or off at runtime.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            if !self.active {
                self.register_service();
            }
        } else {
            self.shutdown();
        }
    }

    /// Updates hostname and/or enabled state live.
    pub fn reconfigure(&mut self, hostname: String, enabled: bool) {
        let normalized = normalize_hostname(&hostname);
        let hostname_changed = normalized != self.hostname;
        self.hostname = normalized;
        self.enabled = enabled;

        if !enabled {
            self.shutdown();
        } else if hostname_changed && self.active {
            self.shutdown();
            self.register_service();
        } else if !self.active {
            self.register_service();
        }
    }

    /// Unregisters the service and shuts down the mDNS daemon cleanly.
    pub fn shutdown(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            if let Some(fullname) = self.fullname.take() {
                if let Ok(rx) = daemon.unregister(&fullname) {
                    let _ = rx.recv_timeout(Duration::from_millis(250));
                }
            }
            if let Ok(rx) = daemon.shutdown() {
                let deadline = std::time::Instant::now() + Duration::from_secs(1);
                while let Some(remaining) =
                    deadline.checked_duration_since(std::time::Instant::now())
                {
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(DaemonStatus::Shutdown) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
            }
        }
        self.fullname = None;
        self.active = false;
    }

    fn register_service(&mut self) {
        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "mDNS daemon failed to start; continuing without mDNS");
                self.active = false;
                return;
            }
        };

        let host_fqdn = ensure_trailing_dot(&self.hostname);
        let properties = [("path", "/"), ("version", truongphoto_core::APP_VERSION)];
        let info = match ServiceInfo::new(
            SERVICE_TYPE,
            INSTANCE_NAME,
            &host_fqdn,
            "",
            self.port,
            &properties[..],
        ) {
            Ok(info) => info.enable_addr_auto(),
            Err(err) => {
                tracing::warn!(error = %err, "failed to construct mDNS ServiceInfo");
                let _ = daemon.shutdown();
                self.active = false;
                return;
            }
        };

        let fullname = info.get_fullname().to_string();
        if let Err(err) = daemon.register(info) {
            tracing::warn!(error = %err, "failed to register mDNS service; continuing without mDNS");
            let _ = daemon.shutdown();
            self.active = false;
            return;
        }

        self.daemon = Some(daemon);
        self.fullname = Some(fullname);
        self.active = true;
    }
}

impl Drop for MdnsHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn ensure_trailing_dot(host: &str) -> String {
    let trimmed = host.trim();
    if trimmed.ends_with('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn normalize_hostname(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "truongphoto.local.".to_string()
    } else {
        format!("{trimmed}.")
    }
}
