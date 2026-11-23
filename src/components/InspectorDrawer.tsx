import React, { useEffect, useState } from 'react';
import {
  Activity,
  ArrowLeft,
  Check,
  Copy,
  Cpu,
  FolderOpen,
  HardDrive,
  Pause,
  Play,
  RefreshCw,
  Server,
  Sparkles,
  Wifi,
  X,
} from 'lucide-react';
import {
  pauseAiIndexing,
  resumeAiIndexing,
  scanAllFolders,
} from '../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { formatBytes, formatRelativeTime, formatUptime } from '../lib/format';
import { openExternalUrl } from '../lib/tauri';
import { notifyError, notifyInfo, notifySuccess } from '../store/feedback';
import { useAppStore } from '../store/useAppStore';
import { AssetInspector } from './AssetInspector';
import { Badge } from './ui';

function formatTierShort(tier: string | undefined): string {
  switch (tier) {
    case 'tier1':
      return 'Fast';
    case 'tier2':
      return 'Balanced';
    case 'tier3':
      return 'Max';
    case 'tier0':
      return 'Classic';
    default:
      return tier ?? '—';
  }
}

export const InspectorDrawer: React.FC<{
  open: boolean;
  closeRef: React.RefObject<HTMLButtonElement | null>;
  onClose: () => void;
}> = ({ open, closeRef, onClose }) => {
  const user = useAppStore((s) => s.user);
  const folders = useAppStore((s) => s.folders);
  const systemInfo = useAppStore((s) => s.systemInfo);
  const liveMetricsStore = useAppStore((s) => (open ? s.liveMetrics : null));
  const powerState = useAppStore((s) => s.powerState);
  const throttleStatus = useAppStore((s) => s.throttleStatus);
  const wsConnected = useAppStore((s) => s.wsConnected);
  const activeJobs = useAppStore((s) => s.activeJobs);
  const aiDashboard = useAppStore((s) => s.aiDashboard);
  const selectedAssetId = useAppStore((s) => s.selectedAssetId);

  // Preserve the last liveMetrics snapshot while the drawer slides closed so numbers don't flash to "—".
  const lastLiveMetricsRef = React.useRef(liveMetricsStore);
  if (open && liveMetricsStore) {
    lastLiveMetricsRef.current = liveMetricsStore;
  }
  const liveMetrics = open ? liveMetricsStore : lastLiveMetricsRef.current;

  const selectAsset = useAppStore((s) => s.selectAsset);
  const refreshSystemInfo = useAppStore((s) => s.refreshSystemInfo);
  const refreshFolders = useAppStore((s) => s.refreshFolders);
  const refreshAiDashboard = useAppStore((s) => s.refreshAiDashboard);

  const isAdmin = user?.role === 'admin';
  const [refreshing, setRefreshing] = useState(false);
  const [togglingAi, setTogglingAi] = useState(false);
  const [scanningAll, setScanningAll] = useState(false);
  const [copiedUrl, setCopiedUrl] = useState<string | null>(null);

  // Keep status & AI telemetry fresh whenever the drawer opens.
  useEffect(() => {
    if (!open) return;
    void refreshSystemInfo().catch(() => {});
    if (isAdmin) {
      void refreshAiDashboard().catch(() => {});
    }
  }, [isAdmin, open, refreshAiDashboard, refreshSystemInfo]);

  useEffect(() => {
    if (!open) return;
    const layer = pushEscapeLayer();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        onClose();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [open, onClose]);

  const effectivePower = powerState ?? systemInfo?.power ?? null;
  const showingAsset = Boolean(selectedAssetId);
  const jobList = Object.values(activeJobs);

  const totalRam = systemInfo?.hardware.total_ram_bytes ?? 0;
  const procRam = liveMetrics?.process_ram_bytes ?? 0;
  const ramPct =
    totalRam > 0 && procRam > 0 ? Math.min(100, Math.max(2, (procRam / totalRam) * 100)) : 0;

  const queue = aiDashboard?.queue ?? null;
  const aiState = aiDashboard?.state ?? null;
  const totalAssets = queue ? queue.indexed_assets + queue.pending_assets : systemInfo?.stats.asset_count ?? 0;
  const indexedAssets = queue?.indexed_assets ?? 0;
  const aiPct = totalAssets > 0 ? Math.min(100, Math.round((indexedAssets / totalAssets) * 100)) : 100;

  const handleRefreshAll = async () => {
    setRefreshing(true);
    try {
      await Promise.all([
        refreshSystemInfo(),
        refreshFolders(),
        isAdmin ? refreshAiDashboard() : Promise.resolve(),
      ]);
    } catch (err) {
      notifyError(err, 'Could not refresh status');
    } finally {
      setRefreshing(false);
    }
  };

  const handleToggleAiPause = async () => {
    if (!isAdmin || !aiState) return;
    setTogglingAi(true);
    try {
      if (aiState.paused) {
        await resumeAiIndexing();
        notifySuccess('Resumed AI analysis');
      } else {
        await pauseAiIndexing();
        notifySuccess('Paused AI analysis');
      }
      await refreshAiDashboard();
    } catch (err) {
      notifyError(err, 'Could not toggle AI indexing');
    } finally {
      setTogglingAi(false);
    }
  };

  const handleScanAll = async () => {
    if (!isAdmin) return;
    setScanningAll(true);
    try {
      const res = await scanAllFolders();
      const count = res.job_ids.length;
      notifySuccess(`Queued scan for ${count} folder${count === 1 ? '' : 's'}`);
    } catch (err) {
      notifyError(err, 'Could not start folder scan');
    } finally {
      setScanningAll(false);
    }
  };

  const handleCopyUrl = async (url: string) => {
    try {
      await navigator.clipboard?.writeText(url);
      setCopiedUrl(url);
      window.setTimeout(() => setCopiedUrl((cur) => (cur === url ? null : cur)), 1800);
    } catch {
      notifyInfo(url);
    }
  };

  const accessUrls: { label: string; url: string }[] = [];
  if (systemInfo?.tailscale_url) {
    accessUrls.push({ label: 'Tailscale HTTPS', url: systemInfo.tailscale_url });
  }
  if (systemInfo?.mdns_url) {
    accessUrls.push({ label: 'Local mDNS', url: systemInfo.mdns_url });
  }
  for (const url of systemInfo?.lan_urls ?? []) {
    accessUrls.push({ label: 'LAN', url });
  }

  return (
    <aside
      className="inspector-drawer status-drawer"
      aria-label={showingAsset ? 'Photo details inspector' : 'Server status and activity'}
    >
      <div className="inspector-heading">
        <div className="row" style={{ gap: 8, minWidth: 0 }}>
          {showingAsset ? (
            <button
              type="button"
              className="button button-quiet"
              style={{ minHeight: 26, padding: '2px 8px', fontSize: 12 }}
              onClick={() => selectAsset(null)}
            >
              <ArrowLeft size={13} />
              <span>Server Status</span>
            </button>
          ) : (
            <span className="row" style={{ gap: 7 }}>
              <span
                className={`status-live-dot${wsConnected ? ' is-online' : ' is-offline'}${jobList.length > 0 ? ' is-busy' : ''}`}
                aria-hidden="true"
              />
              <span>Server Status</span>
            </span>
          )}
        </div>

        <div className="row" style={{ gap: 4 }}>
          {!showingAsset && (
            <button
              type="button"
              className="icon-button"
              aria-label="Refresh server status"
              title="Refresh status"
              disabled={refreshing}
              onClick={() => void handleRefreshAll()}
            >
              <RefreshCw size={14} className={refreshing ? 'spin-icon' : undefined} />
            </button>
          )}
          <button
            type="button"
            ref={closeRef}
            className="icon-button"
            aria-label="Close panel"
            onClick={onClose}
          >
            <X size={16} />
          </button>
        </div>
      </div>

      {showingAsset && selectedAssetId ? (
        <AssetInspector assetId={selectedAssetId} />
      ) : (
        <div className="property-inspector status-drawer-body">
          {/* 1. Server Health Hero Card */}
          <div className="status-hero-card">
            <div className="row" style={{ justifyContent: 'space-between', gap: 8 }}>
              <div className="row" style={{ gap: 8 }}>
                <Server size={15} className="status-section-icon" />
                <div>
                  <div style={{ fontWeight: 600, fontSize: 13 }}>
                    {systemInfo?.hostname || 'TruongPhoto Server'}
                  </div>
                  <div className="view-subtitle" style={{ marginTop: 1, fontSize: 11 }}>
                    v{systemInfo?.version ?? '0.1.0'} · {systemInfo?.platform ?? 'local'} /{' '}
                    {systemInfo?.arch ?? ''}
                  </div>
                </div>
              </div>
              <Badge tone={wsConnected ? 'ok' : 'warn'}>
                {wsConnected ? 'Online' : 'Reconnecting'}
              </Badge>
            </div>

            <div className="status-stat-grid">
              <div className="status-stat-cell">
                <span className="status-stat-label">Library</span>
                <strong className="status-stat-value">
                  {(systemInfo?.stats.asset_count ?? 0).toLocaleString()}
                </strong>
              </div>
              <div className="status-stat-cell">
                <span className="status-stat-label">Database</span>
                <strong className="status-stat-value">
                  {formatBytes(liveMetrics?.db_size_bytes ?? systemInfo?.stats.db_size_bytes ?? 0)}
                </strong>
              </div>
              <div className="status-stat-cell">
                <span className="status-stat-label">Uptime</span>
                <strong className="status-stat-value">
                  {liveMetrics ? formatUptime(liveMetrics.uptime_secs) : '—'}
                </strong>
              </div>
            </div>
          </div>

          {/* 2. Live AI & Background Activity */}
          <section className="status-section">
            <div className="row" style={{ justifyContent: 'space-between', marginBottom: 8 }}>
              <h4 style={{ margin: 0 }} className="row">
                <Sparkles size={12} style={{ marginRight: 5 }} />
                <span>AI &amp; Activity</span>
              </h4>
              {aiState && (
                <Badge
                  tone={
                    aiState.paused
                      ? 'warn'
                      : queue && queue.pending_assets > 0
                        ? 'info'
                        : 'ok'
                  }
                >
                  {aiState.paused
                    ? 'Paused'
                    : queue && queue.pending_assets > 0
                      ? `${queue.pending_assets} left`
                      : 'Up to Date'}
                </Badge>
              )}
            </div>

            {queue && (
              <div className="property-field" style={{ gap: 8 }}>
                <div className="property-row">
                  <span>Photos analyzed</span>
                  <span className="property-value">
                    {indexedAssets.toLocaleString()} / {totalAssets.toLocaleString()} ({aiPct}%)
                  </span>
                </div>
                <div className="status-meter-track" role="progressbar" aria-valuenow={aiPct} aria-valuemin={0} aria-valuemax={100}>
                  <div className="status-meter-fill" style={{ width: `${aiPct}%` }} />
                </div>
                <div className="property-row" style={{ fontSize: 11.5 }}>
                  <span>Models</span>
                  <span className="property-value">
                    Vision {formatTierShort(aiState?.active_vision_tier)} · Chat{' '}
                    {formatTierShort(aiState?.active_chat_tier)}
                  </span>
                </div>
              </div>
            )}

            {jobList.length > 0 && (
              <div className="stack" style={{ gap: 6, marginTop: 8 }}>
                {jobList.map((job) => {
                  const pct =
                    job.total && job.total > 0
                      ? Math.min(100, Math.round((job.processed / job.total) * 100))
                      : null;
                  return (
                    <div key={job.job_id} className="status-job-row">
                      <div className="row" style={{ justifyContent: 'space-between', fontSize: 12 }}>
                        <span className="row" style={{ gap: 6 }}>
                          <Activity size={12} />
                          <span>{job.kind.replace(/_/g, ' ')}</span>
                        </span>
                        <span className="property-value">
                          {job.total ? `${job.processed}/${job.total}` : `${job.processed}`}
                        </span>
                      </div>
                      {pct !== null && (
                        <div className="status-meter-track">
                          <div className="status-meter-fill is-accent" style={{ width: `${pct}%` }} />
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}

            {isAdmin && (
              <div className="row" style={{ gap: 6, marginTop: 10 }}>
                {aiState && (
                  <button
                    type="button"
                    className="button"
                    style={{ flex: 1, justifyContent: 'center' }}
                    disabled={togglingAi}
                    onClick={() => void handleToggleAiPause()}
                  >
                    {aiState.paused ? <Play size={13} /> : <Pause size={13} />}
                    <span>{aiState.paused ? 'Resume AI' : 'Pause AI'}</span>
                  </button>
                )}
                <button
                  type="button"
                  className="button"
                  style={{ flex: 1, justifyContent: 'center' }}
                  disabled={scanningAll}
                  onClick={() => void handleScanAll()}
                >
                  <RefreshCw size={13} className={scanningAll ? 'spin-icon' : undefined} />
                  <span>Rescan</span>
                </button>
              </div>
            )}
          </section>

          {/* 3. Resources & Power */}
          <section className="status-section">
            <h4 className="row">
              <Cpu size={12} style={{ marginRight: 5 }} />
              <span>Resources &amp; Power</span>
            </h4>
            <div className="property-field" style={{ gap: 8 }}>
              <div className="property-row">
                <span>Memory</span>
                <span className="property-value">
                  {procRam > 0 ? formatBytes(procRam) : '—'}
                  {totalRam > 0 ? ` / ${formatBytes(totalRam)}` : ''}
                </span>
              </div>
              <div className="status-meter-track">
                <div className="status-meter-fill" style={{ width: `${ramPct}%` }} />
              </div>
              {systemInfo && (
                <div className="property-row">
                  <span>CPU Cores</span>
                  <span className="property-value">{systemInfo.hardware.cpu_cores} cores</span>
                </div>
              )}
              {effectivePower && (
                <div className="property-row">
                  <span>Power</span>
                  <Badge tone={effectivePower.on_battery ? 'warn' : 'ok'}>
                    {effectivePower.on_battery ? 'On Battery' : 'AC Power'}
                  </Badge>
                </div>
              )}
              {throttleStatus?.active && (
                <div className="property-row">
                  <span>Throttling</span>
                  <Badge tone="warn">{throttleStatus.reason || 'Active'}</Badge>
                </div>
              )}
            </div>
          </section>

          {/* 4. Network & Quick Access */}
          {accessUrls.length > 0 && (
            <section className="status-section">
              <h4 className="row">
                <Wifi size={12} style={{ marginRight: 5 }} />
                <span>Network Access</span>
              </h4>
              <div className="stack" style={{ gap: 6 }}>
                {accessUrls.map((entry) => (
                  <div key={entry.url} className="status-url-row">
                    <button
                      type="button"
                      className="status-url-link"
                      title={entry.url}
                      onClick={() => void openExternalUrl(entry.url)}
                    >
                      <span className="status-url-label">{entry.label}</span>
                      <span className="status-url-text">{entry.url}</span>
                    </button>
                    <button
                      type="button"
                      className="icon-button"
                      style={{ width: 28, height: 28 }}
                      aria-label={`Copy ${entry.label} URL`}
                      title="Copy URL"
                      onClick={() => void handleCopyUrl(entry.url)}
                    >
                      {copiedUrl === entry.url ? <Check size={13} /> : <Copy size={13} />}
                    </button>
                  </div>
                ))}
              </div>
            </section>
          )}

          {/* 5. Watched Folders */}
          <section className="status-section">
            <h4 className="row">
              <FolderOpen size={12} style={{ marginRight: 5 }} />
              <span>Watched Folders ({folders.length})</span>
            </h4>
            {folders.length === 0 ? (
              <div className="view-subtitle">No watched folders configured.</div>
            ) : (
              <div className="stack" style={{ gap: 6 }}>
                {folders.map((folder) => (
                  <div key={folder.id} className="property-field" style={{ gap: 4 }}>
                    <div className="property-row">
                      <span className="row" style={{ gap: 6, minWidth: 0 }}>
                        <HardDrive size={12} style={{ flexShrink: 0, color: 'var(--text-muted)' }} />
                        <span
                          style={{
                            fontWeight: 500,
                            overflow: 'hidden',
                            textOverflow: 'ellipsis',
                            whiteSpace: 'nowrap',
                          }}
                        >
                          {folder.name}
                        </span>
                      </span>
                      <Badge tone={folder.is_offline ? 'warn' : 'ok'}>
                        {folder.is_offline ? 'Offline' : `${folder.asset_count.toLocaleString()}`}
                      </Badge>
                    </div>
                    {folder.last_scanned_at_ms && (
                      <div style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                        Scanned {formatRelativeTime(folder.last_scanned_at_ms)}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </section>
        </div>
      )}
    </aside>
  );
};
