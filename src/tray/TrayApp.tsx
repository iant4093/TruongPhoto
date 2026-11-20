import React, { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { Check, Copy, ExternalLink, Pause, Play, Power, QrCode } from 'lucide-react';
import { Badge, type BadgeTone } from '../components/ui';
import {
  ApiClientError,
  WsClient,
  desktopBootstrap,
  getAiModelsDashboard,
  getSystemInfo,
  pauseAiIndexing,
  resumeAiIndexing,
  systemQrUrl,
} from '../lib/api';
import { formatBytes, formatUptime } from '../lib/format';
import {
  getDesktopBootstrap,
  isTauri,
  openExternalUrl,
  quitApp,
  showMainWindow,
} from '../lib/tauri';
import type {
  AiModelsDashboardResponse,
  DesktopBootstrapInfo,
  PowerState,
  SystemInfoResponse,
  ThrottleStatus,
  WsServerEvent,
} from '../types/api';

interface LiveTrayStats {
  uptimeSecs: number;
  ramBytes: number;
  dbSizeBytes: number;
}

function resolvePowerBadge(
  throttle: ThrottleStatus | null,
  power: PowerState | null,
): { label: string; tone: BadgeTone } {
  if (throttle?.active) {
    switch (throttle.reason) {
      case 'sleep':
        return { label: 'Sleeping', tone: 'warn' };
      case 'battery':
        return { label: 'Paused (Battery)', tone: 'warn' };
      case 'high_cpu':
        return { label: 'Throttled (High CPU)', tone: 'warn' };
      default:
        return { label: 'Throttled', tone: 'warn' };
    }
  }
  if (power?.sleeping) {
    return { label: 'Sleeping', tone: 'warn' };
  }
  if (power?.on_battery || throttle?.on_battery) {
    return { label: 'On Battery', tone: 'warn' };
  }
  return { label: 'AC Power', tone: 'ok' };
}

export const TrayApp: React.FC = () => {
  const [bootstrapInfo, setBootstrapInfo] = useState<DesktopBootstrapInfo | null>(null);
  const [systemInfo, setSystemInfo] = useState<SystemInfoResponse | null>(null);
  const [aiDashboard, setAiDashboard] = useState<AiModelsDashboardResponse | null>(null);
  const [liveStats, setLiveStats] = useState<LiveTrayStats | null>(null);
  const [powerState, setPowerState] = useState<PowerState | null>(null);
  const [throttleState, setThrottleState] = useState<ThrottleStatus | null>(null);
  const [tailscaleUrl, setTailscaleUrl] = useState<string | null>(null);
  const [wsConnected, setWsConnected] = useState(false);
  const [statusLabel, setStatusLabel] = useState<'Online' | 'Starting' | 'PIN Locked'>('Starting');
  const [error, setError] = useState<string | null>(null);
  const [copiedUrl, setCopiedUrl] = useState<string | null>(null);
  const [qrOpen, setQrOpen] = useState(false);
  const [selectedQrUrl, setSelectedQrUrl] = useState<string | null>(null);
  const [togglingAi, setTogglingAi] = useState(false);
  const wsRef = useRef<WsClient | null>(null);

  const connectLiveSocket = useCallback(() => {
    if (wsRef.current) return;
    const client = new WsClient({
      onConnectionChange: (connected) => setWsConnected(connected),
      onEvent: (event: WsServerEvent) => {
        if (event.type === 'heartbeat') {
          setLiveStats({
            uptimeSecs: event.uptime_secs,
            ramBytes: event.process_ram_bytes,
            dbSizeBytes: event.db_size_bytes,
          });
        } else if (event.type === 'power_state_changed') {
          setPowerState(event.power);
        } else if (event.type === 'throttle_state_changed') {
          setThrottleState(event.throttle);
          setPowerState((prev) => ({
            sleeping: prev?.sleeping ?? false,
            display_asleep: prev?.display_asleep ?? false,
            session_locked: prev?.session_locked ?? false,
            on_battery: event.throttle.on_battery,
          }));
        } else if (event.type === 'tailscale_status_changed') {
          setTailscaleUrl(event.status.serve_active ? event.status.https_url : null);
        } else if (event.type === 'ai_state_changed') {
          setAiDashboard((prev) =>
            prev
              ? {
                  ...prev,
                  state: {
                    ...prev.state,
                    active_vision_tier: event.active_vision_tier,
                    staging_vision_tier: event.staging_vision_tier,
                    paused: event.paused,
                    updated_at_ms: Date.now(),
                  },
                }
              : prev,
          );
        }
      },
    });
    wsRef.current = client;
    client.connect();
  }, []);

  const loadTrayState = useCallback(async () => {
    setError(null);
    try {
      if (isTauri()) {
        const info = await getDesktopBootstrap();
        setBootstrapInfo(info);
        await desktopBootstrap(info.desktop_bootstrap_token);
      }
      const sys = await getSystemInfo();
      setSystemInfo(sys);
      setPowerState(sys.power);
      setThrottleState(sys.throttle ?? null);
      setTailscaleUrl(sys.tailscale_url ?? null);
      setStatusLabel('Online');
      connectLiveSocket();

      const ai = await getAiModelsDashboard().catch(() => null);
      if (ai) {
        setAiDashboard(ai);
      }
    } catch (err) {
      if (err instanceof ApiClientError && err.code === 'DESKTOP_PIN_LOCKED') {
        setStatusLabel('PIN Locked');
        setError('Desktop PIN lock is enabled. Unlock in the main window.');
      } else if (err instanceof ApiClientError && err.code === 'NOT_INITIALIZED') {
        setStatusLabel('Starting');
        setError('First-run setup has not been completed yet.');
      } else {
        setStatusLabel('Starting');
        setError(err instanceof Error ? err.message : String(err));
      }
    }
  }, [connectLiveSocket]);

  useEffect(() => {
    void loadTrayState();
    return () => {
      wsRef.current?.disconnect();
      wsRef.current = null;
    };
  }, [loadTrayState]);

  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void getCurrentWebviewWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) {
          void loadTrayState();
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        void getCurrentWebviewWindow().hide().catch(() => {});
      }
    };
    document.addEventListener('keydown', onKeyDown);

    return () => {
      cancelled = true;
      unlisten?.();
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [loadTrayState]);

  const handleCopy = async (url: string) => {
    try {
      await navigator.clipboard?.writeText(url);
      setCopiedUrl(url);
      window.setTimeout(() => setCopiedUrl((cur) => (cur === url ? null : cur)), 2000);
    } catch {
      // Ignore clipboard failure in constrained environments
    }
  };

  const handleToggleAiPause = async () => {
    if (!aiDashboard || togglingAi) return;
    setTogglingAi(true);
    setError(null);
    try {
      const nextState = aiDashboard.state.paused
        ? await resumeAiIndexing()
        : await pauseAiIndexing();
      setAiDashboard((prev) => (prev ? { ...prev, state: nextState } : prev));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setTogglingAi(false);
    }
  };

  const handleOpenMain = async () => {
    if (!isTauri()) return;
    try {
      await showMainWindow();
      await getCurrentWebviewWindow().hide().catch(() => {});
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleQuit = async () => {
    if (!isTauri()) return;
    try {
      await quitApp();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const effectiveTailscaleUrl = tailscaleUrl ?? systemInfo?.tailscale_url ?? null;
  const mdnsUrl = systemInfo?.mdns_url ?? bootstrapInfo?.mdns_url ?? null;
  const rawUrls = [
    ...(effectiveTailscaleUrl ? [effectiveTailscaleUrl] : []),
    ...(mdnsUrl ? [mdnsUrl] : []),
    ...(systemInfo?.lan_urls ?? []),
  ];
  const uniqueUrls = Array.from(new Set(rawUrls));
  const activeQrUrl =
    selectedQrUrl && uniqueUrls.includes(selectedQrUrl) ? selectedQrUrl : (uniqueUrls[0] ?? null);
  const powerBadge = resolvePowerBadge(throttleState, powerState ?? systemInfo?.power ?? null);

  return (
    <div
      className="app-window-frame tray-shell"
      style={{ width: 360, height: 480, padding: 14, gap: 10 }}
    >
      <div className="row" style={{ justifyContent: 'space-between' }}>
        <div className="row">
          <img src="/app-icon.png" width={20} height={20} alt="" />
          <strong style={{ fontSize: 15, letterSpacing: '-0.01em' }}>TruongPhoto</strong>
        </div>
        <Badge
          tone={
            statusLabel === 'Online' ? 'ok' : statusLabel === 'PIN Locked' ? 'warn' : 'muted'
          }
        >
          {statusLabel}
        </Badge>
      </div>

      <div className="card tray-card" style={{ overflowY: 'auto' }}>
        <div className="property-field" style={{ marginBottom: 0 }}>
          <div className="property-row">
            <span>Assets</span>
            <span className="property-value">
              {(systemInfo?.stats.asset_count ?? 0).toLocaleString()}
            </span>
          </div>
          {aiDashboard && (
            <div className="property-row">
              <span>AI Index</span>
              <span className="property-value">
                {aiDashboard.queue.indexed_assets.toLocaleString()} indexed ·{' '}
                {aiDashboard.queue.pending_assets.toLocaleString()} pending
              </span>
            </div>
          )}
          <div className="property-row">
            <span>RAM</span>
            <span className="property-value">
              {liveStats ? formatBytes(liveStats.ramBytes) : '—'}
            </span>
          </div>
          <div className="property-row">
            <span>Database</span>
            <span className="property-value">
              {formatBytes(liveStats?.dbSizeBytes ?? systemInfo?.stats.db_size_bytes ?? 0)}
            </span>
          </div>
          <div className="property-row">
            <span>Uptime</span>
            <span className="property-value">
              {liveStats ? formatUptime(liveStats.uptimeSecs) : '—'}
            </span>
          </div>
          <div className="property-row">
            <span>Telemetry</span>
            <span className="row" style={{ gap: 5 }}>
              <span
                className={`ws-dot ${wsConnected ? 'is-connected' : ''}`}
                aria-hidden="true"
              />
              <span className="property-value">{wsConnected ? 'Live' : 'Offline'}</span>
            </span>
          </div>
          <div className="property-row">
            <span>Power</span>
            <Badge tone={powerBadge.tone}>{powerBadge.label}</Badge>
          </div>
        </div>

        {uniqueUrls.length > 0 && (
          <div style={{ borderTop: '1px solid var(--border)', paddingTop: 8 }}>
            <div className="row" style={{ justifyContent: 'space-between', marginBottom: 4 }}>
              <div className="workspace-label">Network Access</div>
              <button
                type="button"
                className="button"
                style={{ padding: '2px 7px', minHeight: 22, fontSize: 11 }}
                aria-pressed={qrOpen}
                onClick={() => setQrOpen((open) => !open)}
              >
                <QrCode size={11} />
                <span>QR</span>
              </button>
            </div>
            <div className="stack" style={{ gap: 4 }}>
              {uniqueUrls.slice(0, 3).map((url) => {
                const isTailscaleHttps = Boolean(
                  effectiveTailscaleUrl && url === effectiveTailscaleUrl,
                );
                return (
                  <div key={url} className="tray-url-row">
                    <button
                      type="button"
                      className="button button-quiet"
                      style={{
                        padding: '2px 4px',
                        minHeight: 24,
                        fontSize: 11,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        minWidth: 0,
                      }}
                      onClick={() => void openExternalUrl(url)}
                    >
                      <ExternalLink size={11} />
                      <span>{url}</span>
                    </button>
                    <div className="row" style={{ gap: 4, flexShrink: 0 }}>
                      {isTailscaleHttps && <Badge tone="ok">HTTPS</Badge>}
                      {uniqueUrls.length > 1 && (
                        <button
                          type="button"
                          className="icon-button"
                          aria-label={`Select QR for ${url}`}
                          aria-pressed={qrOpen && activeQrUrl === url}
                          onClick={() => {
                            setSelectedQrUrl(url);
                            setQrOpen(true);
                          }}
                        >
                          <QrCode size={11} />
                        </button>
                      )}
                      <button
                        type="button"
                        className="icon-button"
                        aria-label={`Copy ${url}`}
                        onClick={() => void handleCopy(url)}
                      >
                        {copiedUrl === url ? <Check size={12} /> : <Copy size={12} />}
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>

            {qrOpen && activeQrUrl && (
              <div className="tray-qr-box">
                <img
                  className="qr-image"
                  src={systemQrUrl(activeQrUrl)}
                  alt="QR code"
                />
                <div
                  className="view-subtitle"
                  style={{ marginTop: 0, fontSize: 10, textAlign: 'center' }}
                >
                  {activeQrUrl}
                </div>
              </div>
            )}
          </div>
        )}

        {error && (
          <div className="error-banner" role="alert" style={{ marginBottom: 0 }}>
            {error}
          </div>
        )}

        <div className="tray-actions">
          {aiDashboard && (
            <div className="tray-actions-row">
              <button
                type="button"
                className="button"
                disabled={togglingAi}
                onClick={() => void handleToggleAiPause()}
              >
                {aiDashboard.state.paused ? <Play size={13} /> : <Pause size={13} />}
                <span>{aiDashboard.state.paused ? 'Resume AI' : 'Pause AI'}</span>
              </button>
            </div>
          )}
          <div className="tray-actions-row">
            <button
              type="button"
              className="button button-primary"
              onClick={() => void handleOpenMain()}
            >
              Open TruongPhoto
            </button>
            <button type="button" className="button" onClick={() => void handleQuit()}>
              <Power size={13} />
              Quit
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
