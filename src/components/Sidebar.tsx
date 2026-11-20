import React from 'react';
import {
  FolderHeart,
  Images,
  MapPin,
  Settings as SettingsIcon,
  Star,
  Trash2,
  Users,
} from 'lucide-react';
import { formatBytes } from '../lib/format';
import { isTauri } from '../lib/tauri';
import { useAppStore } from '../store/useAppStore';
import type { NavTab, ThrottleStatus } from '../types/api';
import { StatusBanner } from './Feedback';
import { Badge } from './ui';
import { WindowControls, isWindowsPlatform } from './WindowControls';

export const PRIMARY_NAV_ITEMS: ReadonlyArray<
  readonly [NavTab, string, React.ComponentType<{ size?: number; strokeWidth?: number }>]
> = [
  ['timeline', 'Timeline', Images],
  ['people', 'People', Users],
  ['albums', 'Albums', FolderHeart],
  ['favorites', 'Favorites', Star],
  ['map', 'Map', MapPin],
  ['trash', 'Trash', Trash2],
];

const SETTINGS_NAV_ITEM = [
  'settings',
  'Settings',
  SettingsIcon,
] as const;

const nativeMacTitlebar =
  typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
const windowsCaption = !nativeMacTitlebar && isWindowsPlatform();

function formatThrottlePillLabel(throttle: ThrottleStatus): string {
  switch (throttle.reason) {
    case 'battery':
      return 'Paused · Battery';
    case 'high_cpu':
      return 'Throttled · CPU';
    case 'sleep':
      return 'Sleeping';
    default:
      return 'Throttled';
  }
}

export const Sidebar: React.FC = () => {
  const activeNav = useAppStore((s) => s.activeNav);
  const setActiveNav = useAppStore((s) => s.setActiveNav);
  const systemInfo = useAppStore((s) => s.systemInfo);
  const liveMetrics = useAppStore((s) => s.liveMetrics);
  const wsConnected = useAppStore((s) => s.wsConnected);
  const powerState = useAppStore((s) => s.powerState);
  const throttleStatus = useAppStore((s) => s.throttleStatus);

  const assetCount = systemInfo?.stats.asset_count ?? 0;
  const dbBytes = liveMetrics?.db_size_bytes ?? systemInfo?.stats.db_size_bytes ?? 0;
  const ramBytes = liveMetrics?.process_ram_bytes ?? null;
  const onBattery = throttleStatus?.on_battery ?? powerState?.on_battery ?? false;

  const renderNavButton = (
    [key, label, Icon]: readonly [
      NavTab,
      string,
      React.ComponentType<{ size?: number; strokeWidth?: number }>,
    ],
  ) => (
    <button
      key={key}
      type="button"
      aria-current={activeNav === key ? 'page' : undefined}
      title={label}
      className={`nav-item ${activeNav === key ? 'is-active' : ''}`}
      onClick={() => setActiveNav(key)}
    >
      <Icon size={16} strokeWidth={1.6} />
      <span>{label}</span>
    </button>
  );

  return (
    <aside className="app-sidebar">
      {isTauri() && !windowsCaption && (
        <div className="sidebar-titlebar" data-tauri-drag-region>
          {!nativeMacTitlebar && <WindowControls />}
        </div>
      )}
      <div className="app-brand" data-tauri-drag-region>
        <img src="/app-icon.png" width={23} height={23} alt="" />
        <span>TruongPhoto</span>
      </div>
      <nav aria-label="Library navigation" className="sidebar-nav">
        {PRIMARY_NAV_ITEMS.map(renderNavButton)}
        <div className="sidebar-spacer" />
        <StatusBanner />
        <div className="sidebar-resources">
          <div className="resource-row">
            <span>Assets</span>
            <span>{assetCount.toLocaleString()}</span>
          </div>
          <div className="resource-row">
            <span>RAM</span>
            <span aria-live="off">{ramBytes === null ? '—' : formatBytes(ramBytes)}</span>
          </div>
          <div className="resource-row">
            <span>Database</span>
            <span>{formatBytes(dbBytes)}</span>
          </div>
          <div className="resource-row">
            <span>Status</span>
            <span className="row" style={{ gap: 5 }}>
              <span
                className={`ws-dot ${wsConnected ? 'is-connected' : ''}`}
                aria-hidden="true"
              />
              <span>{wsConnected ? 'Live' : 'Offline'}</span>
            </span>
          </div>
          <div className="resource-row">
            <span>Power</span>
            <span>
              {onBattery ? (
                <Badge tone="warn">Battery</Badge>
              ) : (
                <Badge tone="ok">AC</Badge>
              )}
            </span>
          </div>
          {throttleStatus?.active && (
            <div className="throttle-pill" role="status">
              {formatThrottlePillLabel(throttleStatus)}
            </div>
          )}
        </div>
        {renderNavButton(SETTINGS_NAV_ITEM)}
      </nav>
    </aside>
  );
};
