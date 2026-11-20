import type { AssetSummary } from '../types/api';

export interface LayoutHeaderRow {
  type: 'header';
  key: string;
  label: string;
  year: number;
  month: number;
  count: number;
}

export interface LayoutTile {
  asset: AssetSummary;
  width: number;
  height: number;
}

export interface LayoutMediaRow {
  type: 'row';
  key: string;
  height: number;
  items: LayoutTile[];
}

export type JustifiedRow = LayoutHeaderRow | LayoutMediaRow;

export const MIN_ASPECT_RATIO = 0.45;
export const MAX_ASPECT_RATIO = 2.5;
export const DEFAULT_ASPECT_RATIO = 1.333;

const DATE_LABEL_FORMATTER = new Intl.DateTimeFormat('en-US', {
  month: 'short',
  day: 'numeric',
  year: 'numeric',
});

/** Resolves and clamps an asset's aspect ratio into `[0.45, 2.5]`, defaulting to `1.333`. */
export function resolveClampedAspectRatio(asset: AssetSummary): number {
  let raw: number | null = null;
  if (
    asset.aspect_ratio !== null &&
    Number.isFinite(asset.aspect_ratio) &&
    asset.aspect_ratio > 0
  ) {
    raw = asset.aspect_ratio;
  } else if (
    asset.width !== null &&
    asset.height !== null &&
    Number.isFinite(asset.width) &&
    Number.isFinite(asset.height) &&
    asset.width > 0 &&
    asset.height > 0
  ) {
    raw = asset.width / asset.height;
  }

  const ratio = raw ?? DEFAULT_ASPECT_RATIO;
  return Math.min(MAX_ASPECT_RATIO, Math.max(MIN_ASPECT_RATIO, ratio));
}

interface DayGroup {
  dayKey: string;
  label: string;
  year: number;
  month: number;
  items: Array<{ asset: AssetSummary; aspect: number }>;
}

function resolveDayBucket(captureAtMs: number): Omit<DayGroup, 'items'> {
  if (!Number.isFinite(captureAtMs) || captureAtMs <= 0) {
    return { dayKey: 'unknown', label: 'Unknown Date', year: 0, month: 0 };
  }
  const date = new Date(captureAtMs);
  if (Number.isNaN(date.getTime())) {
    return { dayKey: 'unknown', label: 'Unknown Date', year: 0, month: 0 };
  }
  const year = date.getFullYear();
  const month = date.getMonth() + 1;
  const day = date.getDate();
  const dayKey = `${year}-${String(month).padStart(2, '0')}-${String(day).padStart(2, '0')}`;
  return {
    dayKey,
    label: DATE_LABEL_FORMATTER.format(date),
    year,
    month,
  };
}

/**
 * Computes virtualizable header and justified media rows from chronologically sorted assets.
 * Complete rows fill `containerWidth` precisely; the final incomplete row in each group stays
 * left-aligned at `targetRowHeight`.
 */
export function computeJustifiedRows(
  assets: AssetSummary[],
  containerWidth: number,
  targetRowHeight: number,
  gap: number = 4,
): JustifiedRow[] {
  if (assets.length === 0 || !Number.isFinite(containerWidth) || containerWidth <= 0) {
    return [];
  }

  const safeGap = Number.isFinite(gap) && gap >= 0 ? gap : 4;
  const safeTargetHeight =
    Number.isFinite(targetRowHeight) && targetRowHeight > 0 ? targetRowHeight : 210;

  // Group contiguous chronological items sharing the same calendar day.
  const groups: DayGroup[] = [];
  for (const asset of assets) {
    const bucket = resolveDayBucket(asset.capture_at_ms);
    const aspect = resolveClampedAspectRatio(asset);
    const prev = groups[groups.length - 1];
    if (prev && prev.dayKey === bucket.dayKey) {
      prev.items.push({ asset, aspect });
    } else {
      groups.push({ ...bucket, items: [{ asset, aspect }] });
    }
  }

  const rows: JustifiedRow[] = [];

  for (let groupIdx = 0; groupIdx < groups.length; groupIdx += 1) {
    const group = groups[groupIdx];
    rows.push({
      type: 'header',
      key: `header-${group.dayKey}-${groupIdx}`,
      label: group.label,
      year: group.year,
      month: group.month,
      count: group.items.length,
    });

    let pending: Array<{ asset: AssetSummary; aspect: number }> = [];
    let sumAspect = 0;
    let rowIdx = 0;

    for (const item of group.items) {
      pending.push(item);
      sumAspect += item.aspect;

      const totalGaps = safeGap * (pending.length - 1);
      const naturalRowWidth = sumAspect * safeTargetHeight + totalGaps;

      if (naturalRowWidth >= containerWidth) {
        const availableWidth = Math.max(1, containerWidth - totalGaps);
        const rawHeight = availableWidth / sumAspect;
        const rowHeight = Math.min(safeTargetHeight * 1.5, Math.max(1, rawHeight));

        rows.push({
          type: 'row',
          key: `row-${group.dayKey}-${groupIdx}-${rowIdx}`,
          height: rowHeight,
          items: pending.map((entry) => ({
            asset: entry.asset,
            width: entry.aspect * rowHeight,
            height: rowHeight,
          })),
        });

        pending = [];
        sumAspect = 0;
        rowIdx += 1;
      }
    }

    if (pending.length > 0) {
      rows.push({
        type: 'row',
        key: `row-${group.dayKey}-${groupIdx}-${rowIdx}`,
        height: safeTargetHeight,
        items: pending.map((entry) => ({
          asset: entry.asset,
          width: entry.aspect * safeTargetHeight,
          height: safeTargetHeight,
        })),
      });
    }
  }

  return rows;
}

/** Returns the inclusive local end-of-month timestamp in ms for a 1-based month (`1..=12`). */
export function endOfMonthMs(year: number, month: number): number {
  return new Date(year, month, 0, 23, 59, 59, 999).getTime();
}
