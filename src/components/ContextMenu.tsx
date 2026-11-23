import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { create } from 'zustand';
import { RefreshCw, Settings, Sparkles, Upload } from 'lucide-react';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { useAppStore } from '../store/useAppStore';

export type ContextMenuEntry =
  | {
      type?: 'item';
      id: string;
      label: string;
      icon?: React.ReactNode;
      shortcut?: string;
      danger?: boolean;
      disabled?: boolean;
      onSelect: () => void | Promise<void>;
    }
  | {
      type: 'separator';
      id: string;
    };

interface ContextMenuState {
  open: boolean;
  x: number;
  y: number;
  items: ContextMenuEntry[];
  openAt: (x: number, y: number, items: ContextMenuEntry[]) => void;
  close: () => void;
}

export const useContextMenuStore = create<ContextMenuState>((set) => ({
  open: false,
  x: 0,
  y: 0,
  items: [],
  openAt: (x, y, items) => set({ open: true, x, y, items }),
  close: () => set({ open: false, items: [] }),
}));

export function openContextMenu(
  event: React.MouseEvent,
  items: ContextMenuEntry[],
): void {
  event.preventDefault();
  event.stopPropagation();
  if (items.length === 0) return;
  useContextMenuStore.getState().openAt(event.clientX, event.clientY, items);
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (tag === 'TEXTAREA') return true;
  if (tag === 'INPUT') {
    const input = target as HTMLInputElement;
    const type = (input.type || 'text').toLowerCase();
    return ['text', 'search', 'password', 'email', 'url', 'tel', 'number'].includes(type);
  }
  return false;
}

export const ContextMenuHost: React.FC = () => {
  const open = useContextMenuStore((s) => s.open);
  const x = useContextMenuStore((s) => s.x);
  const y = useContextMenuStore((s) => s.y);
  const items = useContextMenuStore((s) => s.items);
  const close = useContextMenuStore((s) => s.close);

  const menuRef = useRef<HTMLDivElement | null>(null);
  const [coords, setCoords] = useState<{ left: number; top: number }>({ left: x, top: y });
  const [activeIdx, setActiveIdx] = useState<number>(-1);

  // Global right-click interceptor: replace raw browser context menu on all non-input surfaces.
  useEffect(() => {
    const onGlobalContextMenu = (event: MouseEvent) => {
      if (event.defaultPrevented) return;
      if (isEditableTarget(event.target)) return;

      event.preventDefault();
      const store = useAppStore.getState();
      if (!store.authenticated) return;

      const canUpload = store.user?.role === 'admin' || store.user?.role === 'contributor';
      const defaultItems: ContextMenuEntry[] = [
        {
          id: 'ctx-ask-ai',
          label: 'Ask AI Assistant',
          icon: <Sparkles size={14} />,
          shortcut:
            typeof navigator !== 'undefined' && /Mac/.test(navigator.platform) ? '⌘K' : 'Ctrl+K',
          onSelect: () => store.setActiveNav('chat'),
        },
        {
          id: 'ctx-refresh',
          label: 'Refresh Library',
          icon: <RefreshCw size={14} />,
          onSelect: () => {
            store.bumpLibraryRevision();
            void store.refreshSystemInfo().catch(() => {});
          },
        },
      ];

      if (canUpload) {
        defaultItems.push({
          id: 'ctx-upload',
          label: 'Upload Photos & Videos…',
          icon: <Upload size={14} />,
          onSelect: () => store.setUploadModalOpen(true),
        });
      }

      defaultItems.push(
        { type: 'separator', id: 'ctx-sep-nav' },
        {
          id: 'ctx-settings',
          label: 'Open Settings',
          icon: <Settings size={14} />,
          onSelect: () => store.setActiveNav('settings'),
        },
      );

      useContextMenuStore.getState().openAt(event.clientX, event.clientY, defaultItems);
    };

    document.addEventListener('contextmenu', onGlobalContextMenu);
    return () => document.removeEventListener('contextmenu', onGlobalContextMenu);
  }, []);

  useLayoutEffect(() => {
    if (!open) return;
    setActiveIdx(-1);
    const el = menuRef.current;
    const width = el?.offsetWidth ?? 216;
    const height = el?.offsetHeight ?? Math.min(340, items.length * 34 + 12);
    const pad = 8;
    const left = Math.max(pad, Math.min(x, window.innerWidth - width - pad));
    const top = Math.max(pad, Math.min(y, window.innerHeight - height - pad));
    setCoords({ left, top });
  }, [open, x, y, items.length]);

  const activeIdxRef = useRef(activeIdx);
  activeIdxRef.current = activeIdx;

  useEffect(() => {
    if (!open) return;
    const layer = pushEscapeLayer();

    const onPointerDown = (event: PointerEvent) => {
      if (event.target instanceof Node && menuRef.current?.contains(event.target)) {
        return;
      }
      close();
    };

    const onScrollOrResize = () => close();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        close();
        return;
      }
      const actionIndices = items
        .map((it, idx) => (it.type !== 'separator' && !it.disabled ? idx : -1))
        .filter((idx) => idx >= 0);
      if (actionIndices.length === 0) return;

      if (event.key === 'ArrowDown') {
        event.preventDefault();
        const curPos = actionIndices.indexOf(activeIdxRef.current);
        const nextPos = curPos < 0 ? 0 : (curPos + 1) % actionIndices.length;
        setActiveIdx(actionIndices[nextPos]);
      } else if (event.key === 'ArrowUp') {
        event.preventDefault();
        const curPos = actionIndices.indexOf(activeIdxRef.current);
        const nextPos =
          curPos < 0 ? actionIndices.length - 1 : (curPos - 1 + actionIndices.length) % actionIndices.length;
        setActiveIdx(actionIndices[nextPos]);
      } else if (event.key === 'Enter' && activeIdxRef.current >= 0) {
        const entry = items[activeIdxRef.current];
        if (entry && entry.type !== 'separator' && !entry.disabled) {
          event.preventDefault();
          close();
          void entry.onSelect();
        }
      }
    };

    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('keydown', onKeyDown);
    window.addEventListener('scroll', onScrollOrResize, true);
    window.addEventListener('resize', onScrollOrResize);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('scroll', onScrollOrResize, true);
      window.removeEventListener('resize', onScrollOrResize);
      popEscapeLayer(layer);
    };
  }, [close, items, open]);

  if (!open || items.length === 0) return null;

  return createPortal(
    <div
      ref={menuRef}
      role="menu"
      aria-label="Context actions"
      className="context-menu"
      style={{ left: coords.left, top: coords.top }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((entry, idx) => {
        if (entry.type === 'separator') {
          return <div key={entry.id} role="separator" className="context-menu-separator" />;
        }
        return (
          <button
            key={entry.id}
            type="button"
            role="menuitem"
            disabled={entry.disabled}
            className={`context-menu-item${entry.danger ? ' is-danger' : ''}${idx === activeIdx ? ' is-highlighted' : ''}`}
            onMouseEnter={() => setActiveIdx(idx)}
            onClick={() => {
              if (entry.disabled) return;
              close();
              void entry.onSelect();
            }}
          >
            <span className="context-menu-item-main">
              {entry.icon && <span className="context-menu-icon">{entry.icon}</span>}
              <span>{entry.label}</span>
            </span>
            {entry.shortcut && <span className="context-menu-shortcut">{entry.shortcut}</span>}
          </button>
        );
      })}
    </div>,
    document.body,
  );
};
