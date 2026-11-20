export const KB = 1024;
export const MB = 1024 * KB;
export const GB = 1024 * MB;
export const TB = 1024 * GB;

/** Formats a byte count into human-readable units (B, KB, MB, GB, TB). */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < KB) return `${Math.round(bytes)} B`;
  if (bytes < MB) return `${(bytes / KB).toFixed(1)} KB`;
  if (bytes < GB) return `${(bytes / MB).toFixed(1)} MB`;
  if (bytes < TB) return `${(bytes / GB).toFixed(2)} GB`;
  return `${(bytes / TB).toFixed(2)} TB`;
}

/** Formats a Unix millisecond timestamp relative to `nowMs` (default: Date.now()). */
export function formatRelativeTime(timestampMs: number, nowMs: number = Date.now()): string {
  if (!Number.isFinite(timestampMs) || timestampMs <= 0) return 'Unknown';
  const diffMs = nowMs - timestampMs;
  const future = diffMs < 0;
  const absSecs = Math.floor(Math.abs(diffMs) / 1000);

  if (absSecs < 10) return 'Just now';
  if (absSecs < 60) return future ? `in ${absSecs}s` : `${absSecs}s ago`;

  const mins = Math.floor(absSecs / 60);
  if (mins < 60) return future ? `in ${mins}m` : `${mins}m ago`;

  const hours = Math.floor(mins / 60);
  if (hours < 24) return future ? `in ${hours}h` : `${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 30) return future ? `in ${days}d` : `${days}d ago`;

  const months = Math.floor(days / 30);
  if (months < 12) return future ? `in ${months}mo` : `${months}mo ago`;

  const years = Math.floor(days / 365);
  return future ? `in ${years}y` : `${years}y ago`;
}

/** Formats an uptime duration in seconds (e.g. "42s", "12m 5s", "3h 14m", "2d 4h"). */
export function formatUptime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0s';
  const total = Math.floor(seconds);
  if (total < 60) return `${total}s`;

  const mins = Math.floor(total / 60);
  const secs = total % 60;
  if (mins < 60) return secs > 0 ? `${mins}m ${secs}s` : `${mins}m`;

  const hours = Math.floor(mins / 60);
  const remMins = mins % 60;
  if (hours < 24) return remMins > 0 ? `${hours}h ${remMins}m` : `${hours}h`;

  const days = Math.floor(hours / 24);
  const remHours = hours % 24;
  return remHours > 0 ? `${days}d ${remHours}h` : `${days}d`;
}

/** Formats a Unix millisecond timestamp as a readable local date & time string. */
export function formatDateTime(timestampMs: number): string {
  if (!Number.isFinite(timestampMs) || timestampMs <= 0) return 'Unknown';
  return new Date(timestampMs).toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export const plural = (count: number, one: string, many = `${one}s`): string =>
  `${count} ${count === 1 ? one : many}`;

/** Passes through standard URLs or resolves local paths safely in Tauri when available. */
export function toAssetUrl(pathOrUrl: string | null | undefined): string | null {
  if (!pathOrUrl) return null;
  if (/^(https?:|data:|blob:|asset:)/i.test(pathOrUrl) || pathOrUrl.startsWith('/')) {
    return pathOrUrl;
  }
  if (typeof window !== 'undefined') {
    const internals = (window as unknown as {
      __TAURI_INTERNALS__?: { convertFileSrc?: (path: string, protocol?: string) => string };
    }).__TAURI_INTERNALS__;
    if (internals && typeof internals.convertFileSrc === 'function') {
      try {
        return internals.convertFileSrc(pathOrUrl);
      } catch {
        return pathOrUrl;
      }
    }
  }
  return pathOrUrl;
}
