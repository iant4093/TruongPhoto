import React, { useEffect, useState } from 'react';
import { Server, X } from 'lucide-react';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { formatBytes, formatRelativeTime, formatUptime } from '../lib/format';
import { useAppStore } from '../store/useAppStore';
import { AssetInspector } from './AssetInspector';
import { Badge } from './ui';

export const InspectorDrawer: React.FC<{
  open: boolean;
  closeRef: React.RefObject<HTMLButtonElement | null>;
  onClose: () => void;
}> = ({ open, closeRef, onClose }) => {
  const user = useAppStore((s) => s.user);
  const allowedFolderIds = useAppStore((s) => s.allowedFolderIds);
  const folders = useAppStore((s) => s.folders);
  const systemInfo = useAppStore((s) => s.systemInfo);
  const liveMetrics = useAppStore((s) => s.liveMetrics);
  const powerState = useAppStore((s) => s.powerState);
  const wsConnected = useAppStore((s) => s.wsConnected);
  const selectedAssetId = useAppStore((s) => s.selectedAssetId);

  const [showTelemetry, setShowTelemetry] = useState(false);

  useEffect(() => {
    if (selectedAssetId) {
      setShowTelemetry(false);
    }
  }, [selectedAssetId]);

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
  const showingAsset = Boolean(selectedAssetId && !showTelemetry);

  return (
    <aside className="inspector-drawer" aria-label="Library and server inspector">
      <div className="inspector-heading">
        <span>{showingAsset ? 'Asset Inspector' : 'Server Telemetry'}</span>
        <div className="row" style={{ gap: 4 }}>
          {selectedAssetId && (
            <button
              type="button"
              className="button button-quiet"
              style={{ minHeight: 24, padding: '2px 8px', fontSize: 11 }}
              onClick={() => setShowTelemetry((prev) => !prev)}
            >
              <Server size={12} />
              <span>{showingAsset ? 'Telemetry' : 'Asset'}</span>
            </button>
          )}
          <button
            type="button"
            ref={closeRef}
            className="icon-button"
            aria-label="Close inspector"
            onClick={onClose}
          >
            <X size={16} />
          </button>
        </div>
      </div>

      {showingAsset && selectedAssetId ? (
        <AssetInspector assetId={selectedAssetId} />
      ) : (
        <div className="property-inspector">
          <section>
            <h4>Session</h4>
            {user ? (
              <>
                <div className="property-field">
                  <div className="property-row">
                    <span>Signed in as</span>
                    <span className="property-value">{user.display_name}</span>
                  </div>
                  <div className="property-row">
                    <span>Username</span>
                    <span className="property-value">@{user.username}</span>
                  </div>
                  <div className="property-row">
                    <span>Role</span>
                    <Badge tone={user.role === 'admin' ? 'info' : 'muted'}>{user.role}</Badge>
                  </div>
                  <div className="property-row">
                    <span>Folder access</span>
                    <span className="property-value">
                      {user.role === 'admin' || allowedFolderIds === null
                        ? 'All folders'
                        : `${allowedFolderIds.length} allowed`}
                    </span>
                  </div>
                </div>
              </>
            ) : (
              <div className="view-subtitle">No active session.</div>
            )}
          </section>

          <section>
            <h4>Watched Folders ({folders.length})</h4>
            {folders.length === 0 ? (
              <div className="view-subtitle">No watched folders configured.</div>
            ) : (
              <div className="stack">
                {folders.map((folder) => (
                  <div key={folder.id} className="property-field">
                    <div className="property-row">
                      <span>{folder.name}</span>
                      <span className="property-value">{folder.asset_count} assets</span>
                    </div>
                    <div className="view-subtitle" style={{ marginTop: 0 }}>
                      {folder.root_path}
                    </div>
                    <div className="row" style={{ flexWrap: 'wrap' }}>
                      {folder.is_offline ? (
                        <Badge tone="warn">Offline</Badge>
                      ) : (
                        <Badge tone="ok">Online</Badge>
                      )}
                      {folder.volume_label && <Badge>{folder.volume_label}</Badge>}
                      {folder.last_scanned_at_ms && (
                        <Badge>Scanned {formatRelativeTime(folder.last_scanned_at_ms)}</Badge>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section>
            <h4>Server &amp; Network</h4>
            {systemInfo ? (
              <div className="property-field">
                <div className="property-row">
                  <span>Version</span>
                  <span className="property-value">{systemInfo.version}</span>
                </div>
                <div className="property-row">
                  <span>Platform</span>
                  <span className="property-value">
                    {systemInfo.platform} / {systemInfo.arch}
                  </span>
                </div>
                <div className="property-row">
                  <span>Hostname</span>
                  <span className="property-value">{systemInfo.hostname}</span>
                </div>
                <div className="property-row">
                  <span>mDNS</span>
                  <Badge tone={systemInfo.mdns_active ? 'ok' : 'muted'}>
                    {systemInfo.mdns_active ? 'Active' : systemInfo.mdns_enabled ? 'Standby' : 'Off'}
                  </Badge>
                </div>
                {systemInfo.mdns_url && (
                  <div className="view-subtitle" style={{ marginTop: 2 }}>
                    {systemInfo.mdns_url}
                  </div>
                )}
                {systemInfo.lan_urls.map((url) => (
                  <div key={url} className="view-subtitle" style={{ marginTop: 0 }}>
                    {url}
                  </div>
                ))}
              </div>
            ) : (
              <div className="view-subtitle">Telemetry unavailable.</div>
            )}
          </section>

          <section>
            <h4>Hardware &amp; Power</h4>
            {systemInfo && (
              <div className="property-field">
                <div className="property-row">
                  <span>System RAM</span>
                  <span className="property-value">
                    {formatBytes(systemInfo.hardware.total_ram_bytes)}
                  </span>
                </div>
                <div className="property-row">
                  <span>CPU Cores</span>
                  <span className="property-value">{systemInfo.hardware.cpu_cores}</span>
                </div>
                <div className="property-row">
                  <span>AI Tiers</span>
                  <span className="property-value">
                    {systemInfo.hardware.recommended_vision_tier} /{' '}
                    {systemInfo.hardware.recommended_chat_tier}
                  </span>
                </div>
              </div>
            )}
            <div className="property-field">
              <div className="property-row">
                <span>WebSocket</span>
                <Badge tone={wsConnected ? 'ok' : 'warn'}>
                  {wsConnected ? 'Connected' : 'Reconnecting'}
                </Badge>
              </div>
              {liveMetrics && (
                <>
                  <div className="property-row">
                    <span>Uptime</span>
                    <span className="property-value">{formatUptime(liveMetrics.uptime_secs)}</span>
                  </div>
                  <div className="property-row">
                    <span>Process RAM</span>
                    <span className="property-value">
                      {formatBytes(liveMetrics.process_ram_bytes)}
                    </span>
                  </div>
                  <div className="property-row">
                    <span>Active Jobs</span>
                    <span className="property-value">{liveMetrics.active_jobs}</span>
                  </div>
                </>
              )}
              {effectivePower && (
                <div className="property-row">
                  <span>Power Source</span>
                  <Badge tone={effectivePower.on_battery ? 'warn' : 'ok'}>
                    {effectivePower.on_battery ? 'On Battery' : 'AC Power'}
                  </Badge>
                </div>
              )}
            </div>
          </section>
        </div>
      )}
    </aside>
  );
};
