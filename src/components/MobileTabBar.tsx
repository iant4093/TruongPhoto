import React, { useEffect, useRef, useState } from 'react';
import {
  FolderHeart,
  Images,
  MapPin,
  MoreHorizontal,
  Settings as SettingsIcon,
  Sparkles,
  Star,
  Trash2,
  Users,
} from 'lucide-react';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { useAppStore } from '../store/useAppStore';
import type { NavTab } from '../types/api';

export const MOBILE_TABS: ReadonlyArray<
  readonly [NavTab, string, React.ComponentType<{ size?: number; strokeWidth?: number }>]
> = [
  ['timeline', 'Timeline', Images],
  ['chat', 'Ask AI', Sparkles],
  ['people', 'People', Users],
  ['albums', 'Albums', FolderHeart],
];

const MORE_TABS: ReadonlyArray<
  readonly [NavTab, string, React.ComponentType<{ size?: number; strokeWidth?: number }>]
> = [
  ['favorites', 'Favorites', Star],
  ['map', 'Map', MapPin],
  ['trash', 'Trash', Trash2],
  ['settings', 'Settings', SettingsIcon],
];

const MORE_TAB_KEYS = new Set<NavTab>(['favorites', 'map', 'trash', 'settings']);

export const MobileTabBar: React.FC<{
  activeNav?: NavTab;
  onSelect?: (nav: NavTab) => void;
}> = ({ activeNav: activeNavProp, onSelect }) => {
  const storeActiveNav = useAppStore((s) => s.activeNav);
  const storeSetActiveNav = useAppStore((s) => s.setActiveNav);
  const [moreOpen, setMoreOpen] = useState(false);
  const barRef = useRef<HTMLElement | null>(null);

  const currentNav = activeNavProp ?? storeActiveNav;
  const selectNav = onSelect ?? storeSetActiveNav;
  const moreActive = MORE_TAB_KEYS.has(currentNav);

  useEffect(() => {
    if (!moreOpen) return;
    const layer = pushEscapeLayer();
    const onPointerDown = (e: Event) => {
      if (e.target instanceof Node && !barRef.current?.contains(e.target)) {
        setMoreOpen(false);
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && isTopEscapeLayer(layer)) {
        e.preventDefault();
        e.stopPropagation();
        setMoreOpen(false);
      }
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [moreOpen]);

  return (
    <nav ref={barRef} aria-label="Mobile navigation" className="mobile-tabbar">
      {moreOpen && (
        <div className="mobile-more-sheet" role="menu" aria-label="More views">
          {MORE_TABS.map(([key, label, Icon]) => {
            const active = currentNav === key;
            return (
              <button
                key={key}
                type="button"
                role="menuitem"
                className={`mobile-more-item${active ? ' is-active' : ''}`}
                aria-current={active ? 'page' : undefined}
                onClick={() => {
                  selectNav(key);
                  setMoreOpen(false);
                }}
              >
                <Icon size={16} strokeWidth={1.6} />
                <span>{label}</span>
              </button>
            );
          })}
        </div>
      )}

      {MOBILE_TABS.map(([key, label, Icon]) => {
        const active = currentNav === key;
        return (
          <button
            key={key}
            type="button"
            className={`mobile-tabbar-item ${active ? 'is-active' : ''}`}
            aria-current={active ? 'page' : undefined}
            onClick={() => {
              setMoreOpen(false);
              selectNav(key);
            }}
          >
            <Icon size={18} strokeWidth={1.6} />
            <span>{label}</span>
          </button>
        );
      })}

      <button
        type="button"
        className={`mobile-tabbar-item ${moreActive || moreOpen ? 'is-active' : ''}`}
        aria-expanded={moreOpen}
        aria-haspopup="menu"
        onClick={() => setMoreOpen((prev) => !prev)}
      >
        <MoreHorizontal size={18} strokeWidth={1.6} />
        <span>More</span>
      </button>
    </nav>
  );
};
