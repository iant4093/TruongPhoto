import React, { useState } from 'react';
import { Image as ImageIcon } from 'lucide-react';
import { toAssetUrl } from '../lib/format';

export const ViewHeader: React.FC<{
  title: string;
  subtitle?: string;
  actions?: React.ReactNode;
}> = ({ title, subtitle, actions }) => (
  <div className="view-header">
    <div>
      <h2>{title}</h2>
      {subtitle && <div className="view-subtitle">{subtitle}</div>}
    </div>
    {actions && <div className="view-actions">{actions}</div>}
  </div>
);

export const SectionTitle: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <div className="section-title">{children}</div>
);

export type BadgeTone = 'cyan' | 'muted' | 'warn' | 'ok' | 'danger' | 'info';

export const Badge: React.FC<{ children: React.ReactNode; tone?: BadgeTone }> = ({
  children,
  tone = 'muted',
}) => {
  const toneClass =
    tone === 'warn'
      ? 'badge-warn'
      : tone === 'ok'
        ? 'badge-ok'
        : tone === 'danger'
          ? 'badge-danger'
          : tone === 'info'
            ? 'badge-info'
            : '';
  return <span className={`badge ${toneClass}`.trim()}>{children}</span>;
};

/** A missing thumbnail remains neutral; changing the URL retries a failed image. */
export const Thumb: React.FC<{
  src: string | null | undefined;
  alt: string;
  height?: number | 'auto';
  contain?: boolean;
  lazy?: boolean;
}> = ({ src, alt, height = 120, contain = false, lazy = false }) => {
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  const url = toAssetUrl(src);
  return (
    <div className={contain ? 'thumbnail thumbnail-contain' : 'thumbnail'} style={{ height }}>
      {url && failedUrl !== url ? (
        <img
          src={url}
          alt={alt}
          decoding="async"
          loading={lazy ? 'lazy' : 'eager'}
          draggable={false}
          onError={() => setFailedUrl(url)}
        />
      ) : (
        <div className="thumbnail-placeholder" role="img" aria-label={alt}>
          <ImageIcon size={22} strokeWidth={1.2} />
        </div>
      )}
    </div>
  );
};

export const ToggleRow: React.FC<{
  label: string;
  description?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
}> = ({ label, description, checked, disabled = false, onChange }) => (
  <label className="toggle-row">
    <span>
      {label}
      {description && <div className="toggle-description">{description}</div>}
    </span>
    <input
      className="toggle-switch"
      type="checkbox"
      checked={checked}
      disabled={disabled}
      onChange={(e) => onChange(e.target.checked)}
    />
  </label>
);

export function SegmentedControl<T extends string>({
  value,
  options,
  onChange,
}: {
  value: T;
  options: ReadonlyArray<readonly [T, string]>;
  onChange: (next: T) => void;
}) {
  return (
    <div className="segmented-control">
      {options.map(([key, label]) => (
        <button
          key={key}
          type="button"
          className="button"
          aria-pressed={value === key}
          onClick={() => onChange(key)}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

export const EmptyState: React.FC<{
  icon?: React.ReactNode;
  title: string;
  description?: string;
  actions?: React.ReactNode;
}> = ({ icon, title, description, actions }) => (
  <div className="empty-state">
    {icon}
    <div className="empty-state-title">{title}</div>
    {description && <div className="empty-state-description">{description}</div>}
    {actions && <div className="empty-actions">{actions}</div>}
  </div>
);

export const cardGrid: React.CSSProperties = {
  display: 'grid',
  gridTemplateColumns: 'repeat(auto-fill, minmax(min(100%, 200px), 1fr))',
  gap: 16,
};
