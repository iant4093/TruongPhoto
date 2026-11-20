import React, { useEffect, useState } from 'react';
import { SlidersHorizontal, X } from 'lucide-react';
import { listPeople } from '../lib/api';
import { notifyError } from '../store/feedback';
import { useAppStore } from '../store/useAppStore';
import type { AssetKind, PersonSummary, SearchMode, SearchRequest } from '../types/api';
import { Dropdown } from './Dropdown';
import { SegmentedControl } from './ui';

const SEARCH_MODES: ReadonlyArray<readonly [SearchMode, string]> = [
  ['hybrid', 'Hybrid'],
  ['semantic', 'Semantic'],
  ['ocr', 'OCR'],
  ['filters', 'Filters'],
];

const KIND_OPTIONS: ReadonlyArray<readonly [AssetKind | '', string]> = [
  ['', 'All Media'],
  ['photo', 'Photo'],
  ['video', 'Video'],
  ['live_photo', 'Live Photo'],
  ['motion_photo', 'Motion Photo'],
  ['raw_jpg_stack', 'RAW + JPG'],
];

function msToDateInput(ms: number | null | undefined): string {
  if (!ms || !Number.isFinite(ms)) return '';
  const d = new Date(ms);
  const year = d.getUTCFullYear();
  const month = String(d.getUTCMonth() + 1).padStart(2, '0');
  const day = String(d.getUTCDate()).padStart(2, '0');
  return `${year}-${month}-${day}`;
}

function parseDateStartMs(value: string): number | null {
  if (!value) return null;
  const parsed = Date.parse(`${value}T00:00:00.000Z`);
  return Number.isFinite(parsed) ? parsed : null;
}

function parseDateEndMs(value: string): number | null {
  if (!value) return null;
  const parsed = Date.parse(`${value}T23:59:59.999Z`);
  return Number.isFinite(parsed) ? parsed : null;
}

function parseTagsInput(raw: string): string[] | null {
  const tags = raw
    .split(',')
    .map((t) => t.trim())
    .filter(Boolean);
  return tags.length > 0 ? tags : null;
}

export const SearchFilterBar: React.FC = () => {
  const searchRequest = useAppStore((s) => s.searchRequest);
  const searchLoading = useAppStore((s) => s.searchLoading);
  const runSearch = useAppStore((s) => s.runSearch);
  const clearSearch = useAppStore((s) => s.clearSearch);

  const [expanded, setExpanded] = useState(true);
  const [people, setPeople] = useState<PersonSummary[]>([]);

  const [cameraInput, setCameraInput] = useState(searchRequest.camera_model ?? '');
  const [geoInput, setGeoInput] = useState(searchRequest.geo_query ?? '');
  const [tagInput, setTagInput] = useState((searchRequest.tags ?? []).join(', '));
  const [dateFromInput, setDateFromInput] = useState(msToDateInput(searchRequest.date_from_ms));
  const [dateToInput, setDateToInput] = useState(msToDateInput(searchRequest.date_to_ms));

  useEffect(() => {
    setCameraInput(searchRequest.camera_model ?? '');
    setGeoInput(searchRequest.geo_query ?? '');
    setTagInput((searchRequest.tags ?? []).join(', '));
    setDateFromInput(msToDateInput(searchRequest.date_from_ms));
    setDateToInput(msToDateInput(searchRequest.date_to_ms));
  }, [
    searchRequest.camera_model,
    searchRequest.geo_query,
    searchRequest.tags,
    searchRequest.date_from_ms,
    searchRequest.date_to_ms,
  ]);

  useEffect(() => {
    let active = true;
    void listPeople()
      .then((res) => {
        if (active) setPeople(res.items);
      })
      .catch(() => {
        // Optional filter options; fail silently if unavailable
      });
    return () => {
      active = false;
    };
  }, []);

  const executeSearch = (patch: Partial<SearchRequest>) => {
    void runSearch({
      camera_model: cameraInput.trim() || null,
      geo_query: geoInput.trim() || null,
      tags: parseTagsInput(tagInput),
      date_from_ms: parseDateStartMs(dateFromInput),
      date_to_ms: parseDateEndMs(dateToInput),
      offset: 0,
      ...patch,
    }).catch((err) => notifyError(err, 'Search failed'));
  };

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    executeSearch({});
  };

  return (
    <form className="search-filter-bar" onSubmit={handleSubmit} aria-label="Search filters">
      <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 8 }}>
        <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
          <SegmentedControl
            value={searchRequest.mode ?? 'hybrid'}
            options={SEARCH_MODES}
            onChange={(mode) => executeSearch({ mode })}
          />
          <button
            type="button"
            className="button"
            aria-expanded={expanded}
            onClick={() => setExpanded((prev) => !prev)}
          >
            <SlidersHorizontal size={13} />
            <span>{expanded ? 'Hide Filters' : 'Filters'}</span>
          </button>
        </div>

        <div className="row" style={{ gap: 8 }}>
          {expanded && (
            <button type="submit" className="button button-primary" disabled={searchLoading}>
              Apply
            </button>
          )}
          <button type="button" className="button button-quiet" onClick={clearSearch}>
            <X size={13} />
            <span>Clear Search</span>
          </button>
        </div>
      </div>

      {expanded && (
        <div className="search-filter-grid">
          <div className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Media Kind</span>
            <Dropdown
              label="Media Kind"
              value={searchRequest.kind ?? ''}
              options={KIND_OPTIONS}
              onChange={(val) => executeSearch({ kind: (val as AssetKind | '') || null })}
            />
          </div>

          <label className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Camera</span>
            <input
              aria-label="Camera"
              className="text-input"
              type="text"
              placeholder="e.g. X-T5, iPhone"
              value={cameraInput}
              onChange={(e) => setCameraInput(e.target.value)}
            />
          </label>

          <label className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Location / Geo</span>
            <input
              aria-label="Location / Geo"
              className="text-input"
              type="text"
              placeholder="City or country"
              value={geoInput}
              onChange={(e) => setGeoInput(e.target.value)}
            />
          </label>

          <label className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Tag</span>
            <input
              aria-label="Tag"
              className="text-input"
              type="text"
              placeholder="e.g. sunset, receipt"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
            />
          </label>

          <div className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Person</span>
            <Dropdown
              label="Person"
              value={searchRequest.person_ids?.[0] ?? ''}
              options={[
                ['', 'Any person'] as const,
                ...people.map((person) => [person.id, person.name || 'Unnamed'] as const),
              ]}
              onChange={(val) =>
                executeSearch({
                  person_ids: val ? [val] : null,
                })
              }
            />
          </div>

          <label className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Date From</span>
            <input
              aria-label="Date From"
              className="text-input"
              type="date"
              value={dateFromInput}
              onChange={(e) => {
                const next = e.target.value;
                setDateFromInput(next);
                executeSearch({ date_from_ms: parseDateStartMs(next) });
              }}
            />
          </label>

          <label className="settings-field" style={{ marginBottom: 0 }}>
            <span className="workspace-label">Date To</span>
            <input
              aria-label="Date To"
              className="text-input"
              type="date"
              value={dateToInput}
              onChange={(e) => {
                const next = e.target.value;
                setDateToInput(next);
                executeSearch({ date_to_ms: parseDateEndMs(next) });
              }}
            />
          </label>

          <label
            className="row"
            style={{
              minHeight: 34,
              cursor: 'pointer',
              fontSize: 12,
              padding: '0 4px',
            }}
          >
            <input
              type="checkbox"
              aria-label="Favorites only"
              checked={Boolean(searchRequest.favorite)}
              onChange={(e) => executeSearch({ favorite: e.target.checked ? true : null })}
            />
            <span>Favorites only</span>
          </label>
        </div>
      )}
    </form>
  );
};
