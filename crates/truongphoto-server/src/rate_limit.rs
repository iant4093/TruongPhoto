use axum::http::HeaderMap;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use truongphoto_core::ClientKind;

const WINDOW_DURATION: Duration = Duration::from_secs(15 * 60);
const STALE_BUCKET_AGE: Duration = Duration::from_secs(30 * 60);
const FREE_ATTEMPTS: u32 = 5;
const MAX_BACKOFF_SECS: u64 = 300;

/// Rate-limiting bucket key derived from peer network identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RateKey {
    TailscaleUser(String),
    ForwardedIp(IpAddr),
    DirectIp(IpAddr),
}

impl RateKey {
    /// Canonical string representation used when binding `tp_gate` cookies to a peer.
    pub fn canonical_str(&self) -> String {
        match self {
            Self::TailscaleUser(login) => format!("ts:{login}"),
            Self::ForwardedIp(ip) => format!("xff:{ip}"),
            Self::DirectIp(ip) => format!("ip:{ip}"),
        }
    }
}

/// Resolved peer identity and trust flags for an incoming HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub peer_ip: IpAddr,
    pub rate_key: RateKey,
    pub is_proxied_tailscale: bool,
    pub is_direct_loopback: bool,
    pub is_https: bool,
}

impl PeerIdentity {
    /// Resolves peer identity from socket peer IP, request headers, and URI scheme.
    ///
    /// Proxy headers (`Tailscale-User-Login`, `X-Forwarded-For`, `X-Forwarded-Proto`)
    /// are honored **only** when the underlying TCP peer is loopback.
    pub fn from_request(peer_ip: IpAddr, headers: &HeaderMap, is_uri_https: bool) -> Self {
        let forwarded_proto_https = peer_ip.is_loopback()
            && headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("https"));
        let is_https = forwarded_proto_https || is_uri_https;

        if !peer_ip.is_loopback() {
            return Self {
                peer_ip,
                rate_key: RateKey::DirectIp(peer_ip),
                is_proxied_tailscale: false,
                is_direct_loopback: false,
                is_https,
            };
        }

        if let Some(login) = headers
            .get("tailscale-user-login")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Self {
                peer_ip,
                rate_key: RateKey::TailscaleUser(login.to_ascii_lowercase()),
                is_proxied_tailscale: true,
                is_direct_loopback: false,
                is_https,
            };
        }

        if let Some(xff_val) = headers.get("x-forwarded-for") {
            // Use the rightmost entry appended by the local trusted reverse proxy so clients
            // cannot spoof a fresh leftmost IP per attempt.
            let rate_key = xff_val
                .to_str()
                .ok()
                .and_then(|raw| raw.rsplit(',').next())
                .map(str::trim)
                .and_then(|last| last.parse::<IpAddr>().ok())
                .map(RateKey::ForwardedIp)
                .unwrap_or(RateKey::DirectIp(peer_ip));

            return Self {
                peer_ip,
                rate_key,
                is_proxied_tailscale: false,
                is_direct_loopback: false,
                is_https,
            };
        }

        Self {
            peer_ip,
            rate_key: RateKey::DirectIp(peer_ip),
            is_proxied_tailscale: false,
            is_direct_loopback: true,
            is_https,
        }
    }

    pub fn client_kind(&self) -> ClientKind {
        if self.is_proxied_tailscale {
            ClientKind::Tailscale
        } else {
            ClientKind::Web
        }
    }

    pub fn ip_string(&self) -> String {
        match &self.rate_key {
            RateKey::ForwardedIp(ip) => ip.to_string(),
            RateKey::DirectIp(ip) => ip.to_string(),
            RateKey::TailscaleUser(login) => format!("tailscale:{login}"),
        }
    }
}

#[derive(Debug, Clone)]
struct Bucket {
    failures: u32,
    window_start: Instant,
    last_attempt: Instant,
    locked_until: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScopedBucketKey {
    scope: String,
    key: RateKey,
}

/// Per-peer, per-stage exponential backoff rate limiter for PIN authentication endpoints.
pub struct AuthRateLimiter {
    buckets: Mutex<HashMap<ScopedBucketKey, Bucket>>,
}

impl AuthRateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Checks whether `key` is currently locked out in the default scope.
    pub fn check(&self, key: &RateKey) -> Result<(), u64> {
        self.check_scoped("default", key)
    }

    /// Checks whether `(scope, key)` is currently locked out.
    pub fn check_scoped(&self, scope: &str, key: &RateKey) -> Result<(), u64> {
        let bucket_key = ScopedBucketKey {
            scope: scope.to_string(),
            key: key.clone(),
        };
        self.check_scoped_at(&bucket_key, Instant::now())
    }

    /// Records a failed attempt for `key` in the default scope.
    pub fn record_failure(&self, key: &RateKey) -> Option<u64> {
        self.record_failure_scoped("default", key)
    }

    /// Records a failed attempt for `(scope, key)`.
    ///
    /// The first 5 failures in a 15-minute window return `None` (0 lock delay).
    /// Failure #6+ sets an exponential backoff lock (`2^k` capped at 300s) and returns `Some(secs)`.
    pub fn record_failure_scoped(&self, scope: &str, key: &RateKey) -> Option<u64> {
        let bucket_key = ScopedBucketKey {
            scope: scope.to_string(),
            key: key.clone(),
        };
        self.record_failure_scoped_at(&bucket_key, Instant::now())
    }

    /// Clears rate-limit state for `key` in the default scope after successful verification.
    pub fn record_success(&self, key: &RateKey) {
        self.record_success_scoped("default", key);
    }

    /// Clears rate-limit state for `(scope, key)` after successful verification.
    pub fn record_success_scoped(&self, scope: &str, key: &RateKey) {
        let bucket_key = ScopedBucketKey {
            scope: scope.to_string(),
            key: key.clone(),
        };
        self.buckets.lock().remove(&bucket_key);
    }

    #[cfg(test)]
    fn check_at(&self, key: &RateKey, now: Instant) -> Result<(), u64> {
        let bucket_key = ScopedBucketKey {
            scope: "default".to_string(),
            key: key.clone(),
        };
        self.check_scoped_at(&bucket_key, now)
    }

    #[cfg(test)]
    fn record_failure_at(&self, key: &RateKey, now: Instant) -> Option<u64> {
        let bucket_key = ScopedBucketKey {
            scope: "default".to_string(),
            key: key.clone(),
        };
        self.record_failure_scoped_at(&bucket_key, now)
    }

    fn check_scoped_at(&self, key: &ScopedBucketKey, now: Instant) -> Result<(), u64> {
        let mut buckets = self.buckets.lock();
        let Some(bucket) = buckets.get(key) else {
            return Ok(());
        };

        if now.duration_since(bucket.window_start) >= WINDOW_DURATION {
            buckets.remove(key);
            return Ok(());
        }

        if let Some(until) = bucket.locked_until {
            if until > now {
                let remaining = until.duration_since(now);
                let secs = remaining
                    .as_secs()
                    .saturating_add(u64::from(remaining.subsec_nanos() > 0))
                    .max(1);
                return Err(secs);
            }
        }

        Ok(())
    }

    fn record_failure_scoped_at(&self, key: &ScopedBucketKey, now: Instant) -> Option<u64> {
        let mut buckets = self.buckets.lock();
        buckets.retain(|_, b| now.duration_since(b.last_attempt) < STALE_BUCKET_AGE);

        let bucket = buckets.entry(key.clone()).or_insert_with(|| Bucket {
            failures: 0,
            window_start: now,
            last_attempt: now,
            locked_until: None,
        });

        if now.duration_since(bucket.window_start) >= WINDOW_DURATION {
            bucket.failures = 0;
            bucket.window_start = now;
            bucket.locked_until = None;
        }

        bucket.failures = bucket.failures.saturating_add(1);
        bucket.last_attempt = now;

        if bucket.failures <= FREE_ATTEMPTS {
            None
        } else {
            let k = bucket.failures - FREE_ATTEMPTS;
            let lock_secs = 2u64.saturating_pow(k.min(9)).min(MAX_BACKOFF_SECS);
            bucket.locked_until = Some(now + Duration::from_secs(lock_secs));
            Some(lock_secs)
        }
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn peer_identity_trusts_proxy_headers_only_on_loopback() {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));

        let mut headers = HeaderMap::new();
        headers.insert("tailscale-user-login", "Alice@Example.com".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());

        let ts_peer = PeerIdentity::from_request(loopback, &headers, false);
        assert_eq!(
            ts_peer.rate_key,
            RateKey::TailscaleUser("alice@example.com".to_string())
        );
        assert!(ts_peer.is_proxied_tailscale);
        assert!(!ts_peer.is_direct_loopback);
        assert!(ts_peer.is_https);

        let lan_spoofed = PeerIdentity::from_request(lan, &headers, false);
        assert_eq!(lan_spoofed.rate_key, RateKey::DirectIp(lan));
        assert!(!lan_spoofed.is_proxied_tailscale);
        assert!(!lan_spoofed.is_direct_loopback);
        assert!(!lan_spoofed.is_https);

        let direct = PeerIdentity::from_request(loopback, &HeaderMap::new(), false);
        assert_eq!(direct.rate_key, RateKey::DirectIp(loopback));
        assert!(direct.is_direct_loopback);
        assert!(!direct.is_https);
    }

    #[test]
    fn rate_limiter_allows_five_failures_then_exponentially_backs_off() {
        let limiter = AuthRateLimiter::new();
        let key = RateKey::DirectIp(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        let base = Instant::now();

        for _ in 0..5 {
            assert!(limiter.check_at(&key, base).is_ok());
            assert_eq!(limiter.record_failure_at(&key, base), None);
        }

        // Failure #6 -> 2s lock
        assert_eq!(limiter.record_failure_at(&key, base), Some(2));
        assert_eq!(limiter.check_at(&key, base), Err(2));

        // After 3s, lock expires; failure #7 -> 4s lock
        let t1 = base + Duration::from_secs(3);
        assert!(limiter.check_at(&key, t1).is_ok());
        assert_eq!(limiter.record_failure_at(&key, t1), Some(4));

        // Success clears bucket
        limiter.record_success(&key);
        assert!(limiter.check_at(&key, t1).is_ok());
    }
}
