import React from 'react';
import {
  FolderHeart,
  Images,
  MapPin,
  Settings as SettingsIcon,
  Users,
} from 'lucide-react';
import { useAppStore } from '../store/useAppStore';
import type { NavTab } from '../types/api';

export const MOBILE_TABS: ReadonlyArray<
  readonly [NavTab, string, React.ComponentType<{ size?: number; strokeWidth?: number }>]
> = [
  ['timeline', 'Timeline', Images],
  ['people', 'People', Users],
  ['albums', 'Albums', FolderHeart],
  ['map', 'Map', MapPin],
  ['settings', 'Settings', SettingsIcon],
];

export const MobileTabBar: React.FC<{
  activeNav?: NavTab;
  onSelect?: (nav: NavTab) => void;
}> = ({ activeNav: activeNavProp, onSelect }) => {
  const storeActiveNav = useAppStore((s) => s.activeNav);
  const storeSetActiveNav = useAppStore((s) => s.setActiveNav);

  const currentNav = activeNavProp ?? storeActiveNav;
  const selectNav = onSelect ?? storeSetActiveNav;

  return (
    <nav aria-label="Mobile navigation" className="mobile-tabbar">
      {MOBILE_TABS.map(([key, label, Icon]) => {
        const active = currentNav === key;
        return (
          <button
            key={key}
            type="button"
            className={`mobile-tabbar-item ${active ? 'is-active' : ''}`}
            aria-current={active ? 'page' : undefined}
            onClick={() => selectNav(key)}
          >
            <Icon size={18} strokeWidth={1.6} />
            <span>{label}</span>
          </button>
        );
      })}
    </nav>
  );
};
