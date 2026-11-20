import React, { useMemo } from 'react';
import type { Histogram256 } from '../types/api';

const SVG_WIDTH = 256;
const SVG_HEIGHT = 84;
const PLOT_HEIGHT = 78;

function buildAreaPolygonPoints(bins: number[], maxVal: number): string {
  if (bins.length === 0) return `0,${SVG_HEIGHT} ${SVG_WIDTH - 1},${SVG_HEIGHT}`;
  const denom = Math.max(1, bins.length - 1);
  const points: string[] = [`0,${SVG_HEIGHT}`];
  for (let i = 0; i < bins.length; i += 1) {
    const x = bins.length === SVG_WIDTH ? i : (i / denom) * (SVG_WIDTH - 1);
    const norm = Math.min(1, Math.max(0, bins[i] / maxVal));
    const y = SVG_HEIGHT - norm * PLOT_HEIGHT;
    points.push(`${x.toFixed(1)},${y.toFixed(1)}`);
  }
  points.push(`${SVG_WIDTH - 1},${SVG_HEIGHT}`);
  return points.join(' ');
}

function buildPolylinePoints(bins: number[], maxVal: number): string {
  if (bins.length === 0) return '';
  const denom = Math.max(1, bins.length - 1);
  const points: string[] = [];
  for (let i = 0; i < bins.length; i += 1) {
    const x = bins.length === SVG_WIDTH ? i : (i / denom) * (SVG_WIDTH - 1);
    const norm = Math.min(1, Math.max(0, bins[i] / maxVal));
    const y = SVG_HEIGHT - norm * PLOT_HEIGHT;
    points.push(`${x.toFixed(1)},${y.toFixed(1)}`);
  }
  return points.join(' ');
}

export interface HistogramChartProps {
  histogram: Histogram256;
  className?: string;
}

export const HistogramChart: React.FC<HistogramChartProps> = ({ histogram, className }) => {
  const { rPoints, gPoints, bPoints, lumaPoints } = useMemo(() => {
    let maxBin = 1;
    for (const channel of [histogram.r, histogram.g, histogram.b, histogram.luma]) {
      for (let i = 0; i < channel.length; i += 1) {
        if (channel[i] > maxBin) maxBin = channel[i];
      }
    }
    return {
      rPoints: buildAreaPolygonPoints(histogram.r, maxBin),
      gPoints: buildAreaPolygonPoints(histogram.g, maxBin),
      bPoints: buildAreaPolygonPoints(histogram.b, maxBin),
      lumaPoints: buildPolylinePoints(histogram.luma, maxBin),
    };
  }, [histogram]);

  return (
    <div className={className ? `histogram-card ${className}` : 'histogram-card'}>
      <svg
        className="histogram-svg"
        width={SVG_WIDTH}
        height={SVG_HEIGHT}
        viewBox={`0 0 ${SVG_WIDTH} ${SVG_HEIGHT}`}
        preserveAspectRatio="none"
        role="img"
        aria-label="RGB histogram"
      >
        <polygon points={rPoints} fill="rgba(235,95,95,0.45)" />
        <polygon points={gPoints} fill="rgba(110,205,130,0.45)" />
        <polygon points={bPoints} fill="rgba(110,165,240,0.45)" />
        {lumaPoints && (
          <polyline
            points={lumaPoints}
            fill="none"
            stroke="rgba(237,237,237,0.7)"
            strokeWidth={1.25}
          />
        )}
      </svg>
    </div>
  );
};
