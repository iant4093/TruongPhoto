import type { DesktopBootstrapInfo, PowerState, UpdateFeedStatus } from '../types/api';

/** True when running inside a Tauri v2 WebView. Safe to call in browsers and Node tests. */
export function isTauri(): boolean {
  if (typeof window === 'undefined') return false;
  return Boolean(
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__,
  );
}

function assertTauri(): void {
  if (!isTauri()) {
    throw new Error('This action is only available in the TruongPhoto desktop app.');
  }
}

async function invokeTauri<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  assertTauri();
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<T>(cmd, args);
}

export function getDesktopBootstrap(): Promise<DesktopBootstrapInfo> {
  return invokeTauri<DesktopBootstrapInfo>('get_desktop_bootstrap');
}

export function getPowerState(): Promise<PowerState> {
  return invokeTauri<PowerState>('get_power_state');
}

export function showMainWindow(): Promise<void> {
  return invokeTauri<void>('show_main_window');
}

export function quitApp(): Promise<void> {
  return invokeTauri<void>('quit_app');
}

export function checkUpdateFeed(): Promise<UpdateFeedStatus> {
  return invokeTauri<UpdateFeedStatus>('check_update_feed');
}

export async function pickFolderDialog(
  title = 'Select folder',
  defaultPath?: string,
): Promise<string | null> {
  if (!isTauri()) return null;
  const { open } = await import('@tauri-apps/plugin-dialog');
  const selected = await open({
    title,
    defaultPath,
    directory: true,
    multiple: false,
    canCreateDirectories: true,
  });
  if (typeof selected === 'string') return selected;
  if (Array.isArray(selected) && typeof selected[0] === 'string') return selected[0];
  return null;
}

export async function getAutostartEnabled(): Promise<boolean> {
  if (!isTauri()) return false;
  const { isEnabled } = await import('@tauri-apps/plugin-autostart');
  return isEnabled();
}

export async function setAutostartEnabled(enabled: boolean): Promise<void> {
  assertTauri();
  const { enable, disable } = await import('@tauri-apps/plugin-autostart');
  if (enabled) {
    await enable();
  } else {
    await disable();
  }
}

export async function openExternalUrl(url: string): Promise<void> {
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-shell');
    await open(url);
    return;
  }
  if (typeof window !== 'undefined') {
    window.open(url, '_blank', 'noopener,noreferrer');
  }
}

export interface NativeDesktopUpdate {
  version: string;
  body: string | null;
  installAndRestart: () => Promise<void>;
}

/**
 * Queries the signed Tauri native updater feed when running inside the desktop app.
 * Returns `null` when running in a browser, when already up to date, or if no signed feed is published.
 */
export async function checkNativeUpdater(): Promise<NativeDesktopUpdate | null> {
  if (!isTauri()) return null;
  try {
    const { check } = await import('@tauri-apps/plugin-updater');
    const update = await check();
    if (!update) return null;
    return {
      version: update.version,
      body: update.body ?? null,
      installAndRestart: async () => {
        await invokeTauri<void>('install_native_update');
      },
    };
  } catch {
    return null;
  }
}

export async function restartApp(): Promise<void> {
  assertTauri();
  const { relaunch } = await import('@tauri-apps/plugin-process');
  await relaunch();
}
