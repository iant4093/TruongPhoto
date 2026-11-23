import React, { useMemo, useRef, useState } from 'react';
import type { TimelineBucket } from '../types/api';

const MONTH_SHORT = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
] as const;

function formatBucketLabel(bucket: TimelineBucket): string {
  const monthName = MONTH_SHORT[bucket.month - 1] ?? String(bucket.month).padStart(2, '0');
  return `${monthName} ${bucket.year} · ${bucket.count}`;
}

export interface TimelineScrubberProps {
  buckets: TimelineBucket[];
  activeYear?: number | null;
  onJumpToMonth: (year: number, month: number) => void;
}

export const TimelineScrubber: React.FC<TimelineScrubberProps> = ({
  buckets,
  activeYear = null,
  onJumpToMonth,
}) => {
  const railRef = useRef<HTMLDivElement | null>(null);
  const [dragging, setDragging] = useState(false);
  const [hoverBucket, setHoverBucket] = useState<TimelineBucket | null>(null);
  const [pillTop, setPillTop] = useState<number>(100);

  const yearMarkers = useMemo(() => {
    const years: number[] = [];
    for (const b of buckets) {
      if (years[years.length - 1] !== b.year && !years.includes(b.year)) {
        years.push(b.year);
      }
    }
    return years;
  }, [buckets]);

  if (buckets.length === 0) return null;

  const resolveBucketAtPointer = (clientY: number): { bucket: TimelineBucket; top: number } => {
    const el = railRef.current;
    if (!el) {
      return { bucket: buckets[0], top: 100 };
    }
    const rect = el.getBoundingClientRect();
    const height = rect.height > 0 ? rect.height : 400;
    const safeClientY = Number.isFinite(clientY) ? clientY : rect.top;
    const relY = Math.min(height, Math.max(0, safeClientY - rect.top));
    const ratio = relY / height;
    const idx = Math.min(buckets.length - 1, Math.max(0, Math.floor(ratio * buckets.length)));
    const clampedTop = Math.min(
      Math.max(rect.top + relY - 14, 60),
      typeof window !== 'undefined' ? window.innerHeight - 48 : 800,
    );
    return { bucket: buckets[idx], top: clampedTop };
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const { bucket, top } = resolveBucketAtPointer(e.clientY);
    setHoverBucket(bucket);
    setPillTop(top);
  };

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    setDragging(true);
    e.currentTarget.setPointerCapture?.(e.pointerId);
    const { bucket, top } = resolveBucketAtPointer(e.clientY);
    setHoverBucket(bucket);
    setPillTop(top);
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    const { bucket } = resolveBucketAtPointer(e.clientY);
    setDragging(false);
    if (e.currentTarget.hasPointerCapture?.(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    onJumpToMonth(bucket.year, bucket.month);
  };

  const handlePointerCancel = (e: React.PointerEvent<HTMLDivElement>) => {
    setDragging(false);
    setHoverBucket(null);
    if (e.currentTarget.hasPointerCapture?.(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  };

  const activeIdx = useMemo(() => {
    if (hoverBucket) {
      const found = buckets.findIndex(
        (b) => b.year === hoverBucket.year && b.month === hoverBucket.month,
      );
      if (found >= 0) return found;
    }
    if (activeYear !== null) {
      const found = buckets.findIndex((b) => b.year === activeYear);
      if (found >= 0) return found;
    }
    return 0;
  }, [activeYear, buckets, hoverBucket]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    let nextIdx: number | null = null;
    if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
      nextIdx = Math.min(buckets.length - 1, activeIdx + 1);
    } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
      nextIdx = Math.max(0, activeIdx - 1);
    } else if (e.key === 'PageDown') {
      nextIdx = Math.min(buckets.length - 1, activeIdx + 6);
    } else if (e.key === 'PageUp') {
      nextIdx = Math.max(0, activeIdx - 6);
    } else if (e.key === 'Home') {
      nextIdx = 0;
    } else if (e.key === 'End') {
      nextIdx = buckets.length - 1;
    }
    if (nextIdx === null) return;
    e.preventDefault();
    const target = buckets[nextIdx];
    if (target) {
      setHoverBucket(target);
      onJumpToMonth(target.year, target.month);
    }
  };

  return (
    <>
      <div
        ref={railRef}
        className="timeline-scrubber"
        role="slider"
        aria-label="Timeline chronological scrubber"
        aria-valuemin={0}
        aria-valuemax={Math.max(0, buckets.length - 1)}
        aria-valuenow={activeIdx}
        aria-valuetext={
          hoverBucket
            ? formatBucketLabel(hoverBucket)
            : buckets[activeIdx]
              ? formatBucketLabel(buckets[activeIdx])
              : undefined
        }
        tabIndex={0}
        onKeyDown={handleKeyDown}
        onPointerMove={handlePointerMove}
        onPointerDown={handlePointerDown}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerCancel}
        onPointerLeave={() => {
          if (!dragging) setHoverBucket(null);
        }}
      >
        {yearMarkers.map((year) => (
          <span
            key={year}
            className={`scrubber-marker${activeYear === year ? ' is-active' : ''}`}
          >
            {year}
          </span>
        ))}
      </div>

      {hoverBucket && (
        <div
          className="scrubber-pill"
          style={{ top: pillTop }}
          role="status"
          aria-live="polite"
        >
          {formatBucketLabel(hoverBucket)}
        </div>
      )}
    </>
  );
};
