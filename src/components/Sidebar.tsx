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
import { isTauri } from '../lib/tauri';
import { useAppStore } from '../store/useAppStore';
import type { NavTab, ThrottleStatus } from '../types/api';
import { StatusBanner } from './Feedback';
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
  const wsConnected = useAppStore((s) => s.wsConnected);
  const throttleStatus = useAppStore((s) => s.throttleStatus);
  const activeJobs = useAppStore((s) => s.activeJobs);

  const assetCount = systemInfo?.stats.asset_count ?? 0;
  const activeJobCount = Object.keys(activeJobs).length;

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
            <span>Photos</span>
            <span>{assetCount.toLocaleString()}</span>
          </div>
          <div className="resource-row">
            <span>Status</span>
            <span className="row" style={{ gap: 5 }}>
              <span
                className={`ws-dot ${wsConnected ? 'is-connected' : ''}`}
                aria-hidden="true"
              />
              <span>{activeJobCount > 0 ? 'Indexing…' : wsConnected ? 'Ready' : 'Offline'}</span>
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
