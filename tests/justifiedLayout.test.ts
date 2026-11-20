import { describe, expect, it } from 'vitest';
import {
  DEFAULT_ASPECT_RATIO,
  MAX_ASPECT_RATIO,
  MIN_ASPECT_RATIO,
  computeJustifiedRows,
  endOfMonthMs,
  resolveClampedAspectRatio,
} from '../src/lib/justifiedLayout';
import type { AssetSummary } from '../src/types/api';

function makeAsset(
  id: string,
  captureAtMs: number,
  aspectRatio: number | null = 1.5,
  dims: { width?: number | null; height?: number | null } = {},
): AssetSummary {
  return {
    id,
    folder_id: 'f-1',
    kind: 'photo',
    capture_at_ms: captureAtMs,
    width: dims.width ?? null,
    height: dims.height ?? null,
    duration_ms: null,
    aspect_ratio: aspectRatio,
    is_favorite: false,
    deleted_at_ms: null,
    thumb_rel_path: null,
    thumb_ready: true,
    title: null,
    rating: null,
    gps_lat: null,
    gps_lon: null,
    geo_city: null,
    geo_country: null,
    camera_model: null,
  };
}

describe('justifiedLayout.ts', () => {
  it('returns an empty array for empty input or non-positive container width', () => {
    const asset = makeAsset('a1', new Date(2026, 10, 19, 12, 0, 0).getTime(), 1.5);
    expect(computeJustifiedRows([], 1200, 210)).toEqual([]);
    expect(computeJustifiedRows([asset], 0, 210)).toEqual([]);
    expect(computeJustifiedRows([asset], -100, 210)).toEqual([]);
  });

  it('groups assets by calendar day with formatted date headers and counts', () => {
    const day1a = new Date(2026, 10, 19, 14, 0, 0).getTime();
    const day1b = new Date(2026, 10, 19, 9, 30, 0).getTime();
    const day2 = new Date(2026, 10, 18, 18, 0, 0).getTime();

    const rows = computeJustifiedRows(
      [makeAsset('a1', day1a, 1.5), makeAsset('a2', day1b, 1.0), makeAsset('a3', day2, 1.5)],
      1200,
      210,
      4,
    );

    const headers = rows.filter((r) => r.type === 'header');
    expect(headers).toHaveLength(2);
    expect(headers[0]).toMatchObject({
      type: 'header',
      label: 'Nov 19, 2026',
      year: 2026,
      month: 11,
      count: 2,
    });
    expect(headers[1]).toMatchObject({
      type: 'header',
      label: 'Nov 18, 2026',
      year: 2026,
      month: 11,
      count: 1,
    });
  });

  it('packs variable aspect ratios to fill exact containerWidth within 1px tolerance', () => {
    const day = new Date(2026, 10, 19, 12, 0, 0).getTime();
    const ratios = [1.5, 0.75, 1.77, 1.33, 0.8, 1.5, 2.0, 1.25, 1.5];
    const assets = ratios.map((r, idx) => makeAsset(`a-${idx}`, day - idx * 1000, r));
    const containerWidth = 1000;
    const gap = 4;

    const rows = computeJustifiedRows(assets, containerWidth, 210, gap);
    const mediaRows = rows.filter((r) => r.type === 'row');
    expect(mediaRows.length).toBeGreaterThanOrEqual(2);

    // Every complete row (all except possibly the last incomplete row) fills containerWidth within 1px
    for (let i = 0; i < mediaRows.length - 1; i += 1) {
      const row = mediaRows[i];
      const totalWidth =
        row.items.reduce((acc, tile) => acc + tile.width, 0) + gap * (row.items.length - 1);
      expect(Math.abs(totalWidth - containerWidth)).toBeLessThanOrEqual(1);
      expect(row.height).toBeLessThanOrEqual(210);
      expect(row.height).toBeGreaterThan(80);
    }
  });

  it('clamps extreme aspect ratios to [0.45, 2.5] and defaults null/invalid to 1.333', () => {
    const ts = new Date(2026, 5, 10, 12, 0, 0).getTime();

    const ultraTall = makeAsset('tall', ts, 0.1);
    const ultraWide = makeAsset('wide', ts, 8.0);
    const missing = makeAsset('missing', ts, null);
    const invalid = makeAsset('invalid', ts, -2);
    const fromDims = makeAsset('dims', ts, null, { width: 1000, height: 500 });

    expect(resolveClampedAspectRatio(ultraTall)).toBe(MIN_ASPECT_RATIO);
    expect(resolveClampedAspectRatio(ultraWide)).toBe(MAX_ASPECT_RATIO);
    expect(resolveClampedAspectRatio(missing)).toBe(DEFAULT_ASPECT_RATIO);
    expect(resolveClampedAspectRatio(invalid)).toBe(DEFAULT_ASPECT_RATIO);
    expect(resolveClampedAspectRatio(fromDims)).toBe(2);

    const rows = computeJustifiedRows([ultraTall, ultraWide, missing], 1400, 200, 4);
    const row = rows.find((r) => r.type === 'row');
    expect(row).toBeDefined();
    if (row && row.type === 'row') {
      expect(row.items[0].width / row.items[0].height).toBeCloseTo(0.45, 4);
      expect(row.items[1].width / row.items[1].height).toBeCloseTo(2.5, 4);
      expect(row.items[2].width / row.items[2].height).toBeCloseTo(1.333, 4);
    }
  });

  it('keeps the final orphan row left-aligned at targetRowHeight without stretching', () => {
    const ts = new Date(2026, 10, 19, 12, 0, 0).getTime();
    const orphan = makeAsset('orphan', ts, 1.5);
    const rows = computeJustifiedRows([orphan], 1400, 210, 4);

    expect(rows).toHaveLength(2);
    const mediaRow = rows[1];
    expect(mediaRow.type).toBe('row');
    if (mediaRow.type === 'row') {
      expect(mediaRow.height).toBe(210);
      expect(mediaRow.items).toHaveLength(1);
      expect(mediaRow.items[0].height).toBe(210);
      expect(mediaRow.items[0].width).toBeCloseTo(315, 2);
      expect(mediaRow.items[0].width).toBeLessThan(1400);
    }
  });

  it('supports distinct mobile (130px) and desktop (210px) target row heights', () => {
    const ts = new Date(2026, 10, 19, 12, 0, 0).getTime();
    const assets = [
      makeAsset('m1', ts, 1.5),
      makeAsset('m2', ts - 1000, 1.5),
      makeAsset('m3', ts - 2000, 1.5),
    ];

    const mobileRows = computeJustifiedRows(assets, 360, 130, 4).filter((r) => r.type === 'row');
    const desktopRows = computeJustifiedRows(assets, 1400, 210, 4).filter((r) => r.type === 'row');

    expect(mobileRows.length).toBeGreaterThanOrEqual(2);
    expect(mobileRows[0].height).toBeLessThanOrEqual(130);
    expect(desktopRows).toHaveLength(1);
    expect(desktopRows[0].height).toBe(210);
  });

  it('computes accurate endOfMonthMs timestamps including December and leap years', () => {
    const novEnd = new Date(endOfMonthMs(2026, 11));
    expect(novEnd.getFullYear()).toBe(2026);
    expect(novEnd.getMonth()).toBe(10);
    expect(novEnd.getDate()).toBe(30);
    expect(novEnd.getHours()).toBe(23);
    expect(novEnd.getMinutes()).toBe(59);
    expect(novEnd.getSeconds()).toBe(59);
    expect(novEnd.getMilliseconds()).toBe(999);

    const decEnd = new Date(endOfMonthMs(2026, 12));
    expect(decEnd.getFullYear()).toBe(2026);
    expect(decEnd.getMonth()).toBe(11);
    expect(decEnd.getDate()).toBe(31);

    const febLeap = new Date(endOfMonthMs(2024, 2));
    expect(febLeap.getFullYear()).toBe(2024);
    expect(febLeap.getMonth()).toBe(1);
    expect(febLeap.getDate()).toBe(29);

    const febNonLeap = new Date(endOfMonthMs(2025, 2));
    expect(febNonLeap.getFullYear()).toBe(2025);
    expect(febNonLeap.getMonth()).toBe(1);
    expect(febNonLeap.getDate()).toBe(28);
  });
});
