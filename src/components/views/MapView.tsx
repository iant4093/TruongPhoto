import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  LngLatBounds,
  Map as MapLibreMap,
  Marker,
  addProtocol,
  setWorkerUrl,
  type GeoJSONSourceSpecification,
  type StyleSpecification,
} from 'maplibre-gl';
import maplibreWorkerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?url';
import 'maplibre-gl/dist/maplibre-gl.css';
import { Protocol } from 'pmtiles';
import { Globe, MapPin, Maximize2, X } from 'lucide-react';
import { getMapClusters, getMapConfig, getMapPoints, mediaThumbUrl } from '../../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../../lib/escapeStack';
import { formatDateTime, plural } from '../../lib/format';
import { WORLD_GRATICULE_GEOJSON, WORLD_LAND_GEOJSON } from '../../lib/worldGeoJson';
import { notifyError } from '../../store/feedback';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary, MapCluster, MapConfigResponse } from '../../types/api';
import { Badge, EmptyState } from '../ui';

const INITIAL_BBOX = {
  min_lat: -85,
  max_lat: 85,
  min_lng: -180,
  max_lng: 180,
  zoom: 2,
};

let pmtilesProtocolRegistered = false;
let cachedWebGLSupport: boolean | null = null;

export function hasWebGLSupport(): boolean {
  if (typeof document === 'undefined') return false;
  if (typeof navigator !== 'undefined' && /jsdom/i.test(navigator.userAgent)) return false;
  if (cachedWebGLSupport !== null) return cachedWebGLSupport;
  try {
    const canvas = document.createElement('canvas');
    const gl = (canvas.getContext('webgl2') || canvas.getContext('webgl')) as
      | WebGLRenderingContext
      | WebGL2RenderingContext
      | null;
    cachedWebGLSupport = Boolean(gl);
    gl?.getExtension('WEBGL_lose_context')?.loseContext();
    return cachedWebGLSupport;
  } catch {
    cachedWebGLSupport = false;
    return false;
  }
}

function ensurePmtilesProtocol(): void {
  if (pmtilesProtocolRegistered) return;
  if (typeof maplibreWorkerUrl === 'string' && maplibreWorkerUrl.length > 0) {
    setWorkerUrl(maplibreWorkerUrl);
  }
  const protocol = new Protocol();
  addProtocol('pmtiles', protocol.tile);
  pmtilesProtocolRegistered = true;
}

function resolvePmtilesSourceUrl(pmtilesPath: string): string {
  if (/^https?:\/\//i.test(pmtilesPath)) {
    return `pmtiles://${pmtilesPath}`;
  }
  const origin = typeof window !== 'undefined' ? window.location.origin : '';
  return `pmtiles://${origin}${pmtilesPath}`;
}

export function buildMapStyle(config: MapConfigResponse): StyleSpecification | string {
  if (config.tile_mode === 'online_openfreemap') {
    return config.online_style_url;
  }

  if (config.pmtiles_url) {
    return {
      version: 8,
      sources: {
        protomaps: {
          type: 'vector',
          url: resolvePmtilesSourceUrl(config.pmtiles_url),
        },
      },
      layers: [
        {
          id: 'background',
          type: 'background',
          paint: { 'background-color': '#171717' },
        },
        {
          id: 'earth',
          type: 'fill',
          source: 'protomaps',
          'source-layer': 'earth',
          paint: { 'fill-color': '#242424' },
        },
        {
          id: 'water',
          type: 'fill',
          source: 'protomaps',
          'source-layer': 'water',
          paint: { 'fill-color': '#171717' },
        },
        {
          id: 'boundaries',
          type: 'line',
          source: 'protomaps',
          'source-layer': 'boundaries',
          paint: { 'line-color': '#343434', 'line-width': 1 },
        },
        {
          id: 'roads',
          type: 'line',
          source: 'protomaps',
          'source-layer': 'roads',
          paint: { 'line-color': '#343434', 'line-width': 0.8 },
        },
        {
          id: 'buildings',
          type: 'fill',
          source: 'protomaps',
          'source-layer': 'buildings',
          paint: { 'fill-color': '#2a2a2a' },
        },
      ],
    };
  }

  return {
    version: 8,
    sources: {
      'world-land': {
        type: 'geojson',
        data: WORLD_LAND_GEOJSON as unknown as GeoJSONSourceSpecification['data'],
      },
      'world-graticule': {
        type: 'geojson',
        data: WORLD_GRATICULE_GEOJSON as unknown as GeoJSONSourceSpecification['data'],
      },
    },
    layers: [
      {
        id: 'ocean-bg',
        type: 'background',
        paint: { 'background-color': '#171717' },
      },
      {
        id: 'graticule-lines',
        type: 'line',
        source: 'world-graticule',
        paint: { 'line-color': '#232323', 'line-width': 0.75 },
      },
      {
        id: 'land-fill',
        type: 'fill',
        source: 'world-land',
        paint: { 'fill-color': '#242424' },
      },
      {
        id: 'land-coastline',
        type: 'line',
        source: 'world-land',
        paint: { 'line-color': '#343434', 'line-width': 1 },
      },
    ],
  };
}

function clusterDisplayLabel(cluster: MapCluster): string {
  if (cluster.label && cluster.label.trim()) {
    return cluster.label;
  }
  return `${cluster.lat.toFixed(2)}, ${cluster.lon.toFixed(2)}`;
}

function formatModePillLabel(config: MapConfigResponse | null): string {
  if (!config) return 'Offline Map';
  if (config.tile_mode === 'online_openfreemap') {
    return 'Online (OpenFreeMap)';
  }
  if (config.pmtiles_url) {
    return 'Offline (PMTiles)';
  }
  return 'Offline Map';
}

export const MapView: React.FC = () => {
  const openLightbox = useAppStore((s) => s.openLightbox);
  const libraryRevision = useAppStore((s) => s.libraryRevision);

  const webglAvailable = useMemo(() => hasWebGLSupport(), []);

  const [config, setConfig] = useState<MapConfigResponse | null>(null);
  const [clusters, setClusters] = useState<MapCluster[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [selectedCluster, setSelectedCluster] = useState<MapCluster | null>(null);
  const [stripAssets, setStripAssets] = useState<AssetSummary[]>([]);
  const [loadingStrip, setLoadingStrip] = useState(false);

  const mapContainerRef = useRef<HTMLDivElement | null>(null);
  const mapInstanceRef = useRef<MapLibreMap | null>(null);
  const [mapInstanceVersion, setMapInstanceVersion] = useState(0);
  const markersRef = useRef<Marker[]>([]);
  const pointsSeqRef = useRef(0);
  const clustersSeqRef = useRef(0);

  const loadInitialMap = useCallback(async (forceWorldBounds = false) => {
    setLoading(true);
    setError(null);
    const seq = ++clustersSeqRef.current;
    try {
      const cfg = await getMapConfig();
      const map = !forceWorldBounds ? mapInstanceRef.current : null;
      const queryBbox = map
        ? (() => {
            const bounds = map.getBounds();
            return {
              min_lat: Math.max(-85, bounds.getSouth()),
              max_lat: Math.min(85, bounds.getNorth()),
              min_lng: Math.max(-180, bounds.getWest()),
              max_lng: Math.min(180, bounds.getEast()),
              zoom: Math.max(0, Math.min(20, Math.round(map.getZoom()))),
            };
          })()
        : INITIAL_BBOX;
      const clusterRes = await getMapClusters(queryBbox);
      if (seq !== clustersSeqRef.current) return;
      setConfig(cfg);
      setClusters(clusterRes.clusters);
    } catch (err) {
      if (seq !== clustersSeqRef.current) return;
      setError(errorMessage(err));
    } finally {
      if (seq === clustersSeqRef.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void loadInitialMap();
  }, [loadInitialMap, libraryRevision]);

  const handleSelectCluster = useCallback(async (cluster: MapCluster) => {
    setSelectedCluster(cluster);
    setLoadingStrip(true);
    const seq = ++pointsSeqRef.current;

    const map = mapInstanceRef.current;
    if (map) {
      const currentZoom = map.getZoom();
      if (currentZoom < 14) {
        map.flyTo({
          center: [cluster.lon, cluster.lat],
          zoom: Math.min(14, Math.max(Math.floor(currentZoom) + 2, 6)),
        });
      }
    }

    try {
      const res = await getMapPoints({
        min_lat: cluster.min_lat,
        max_lat: cluster.max_lat,
        min_lng: cluster.min_lon,
        max_lng: cluster.max_lon,
        limit: 100,
      });
      if (seq !== pointsSeqRef.current) return;
      setStripAssets(res.items);
    } catch (err) {
      if (seq !== pointsSeqRef.current) return;
      notifyError(err, 'Could not load cluster photos');
    } finally {
      if (seq === pointsSeqRef.current) {
        setLoadingStrip(false);
      }
    }
  }, []);

  const handleFitAll = useCallback(() => {
    const map = mapInstanceRef.current;
    if (map && clusters.length > 0) {
      const bounds = new LngLatBounds();
      for (const c of clusters) {
        bounds.extend([c.lon, c.lat]);
      }
      map.fitBounds(bounds, { padding: 64, maxZoom: 12 });
      return;
    }
    if (map) {
      map.flyTo({ center: [0, 20], zoom: 2 });
    }
    setSelectedCluster(null);
    setStripAssets([]);
    void loadInitialMap(true);
  }, [clusters, loadInitialMap]);

  const mapTileMode = config?.tile_mode ?? null;
  const mapPmtilesUrl = config?.pmtiles_url ?? null;
  const mapOnlineStyleUrl = config?.online_style_url ?? null;

  // Initialize MapLibre GL JS only when WebGL is supported or map style source actually changes
  useEffect(() => {
    if (!webglAvailable || !mapTileMode || !mapOnlineStyleUrl || !mapContainerRef.current) return;

    ensurePmtilesProtocol();

    const styleConfig: MapConfigResponse = {
      tile_mode: mapTileMode,
      available_pmtiles: [],
      pmtiles_url: mapPmtilesUrl,
      online_style_url: mapOnlineStyleUrl,
      total_geotagged: 0,
    };

    const map = new MapLibreMap({
      container: mapContainerRef.current,
      style: buildMapStyle(styleConfig),
      center: [0, 20],
      zoom: 2,
      attributionControl: false,
    });
    mapInstanceRef.current = map;
    setMapInstanceVersion((v) => v + 1);

    const onMoveEnd = () => {
      const bounds = map.getBounds();
      const zoom = Math.max(0, Math.min(20, Math.round(map.getZoom())));
      const seq = ++clustersSeqRef.current;
      void getMapClusters({
        min_lat: Math.max(-85, bounds.getSouth()),
        max_lat: Math.min(85, bounds.getNorth()),
        min_lng: Math.max(-180, bounds.getWest()),
        max_lng: Math.min(180, bounds.getEast()),
        zoom,
      })
        .then((res) => {
          if (seq === clustersSeqRef.current) {
            setClusters(res.clusters);
          }
        })
        .catch(() => {});
    };

    map.on('moveend', onMoveEnd);

    return () => {
      map.off('moveend', onMoveEnd);
      for (const marker of markersRef.current) {
        marker.remove();
      }
      markersRef.current = [];
      map.remove();
      mapInstanceRef.current = null;
    };
  }, [webglAvailable, mapTileMode, mapPmtilesUrl, mapOnlineStyleUrl]);

  // Sync Codex DOM cluster markers onto MapLibre map
  useEffect(() => {
    const map = mapInstanceRef.current;
    if (!webglAvailable || !map) return;

    for (const marker of markersRef.current) {
      marker.remove();
    }
    markersRef.current = [];

    for (const cluster of clusters) {
      const el = document.createElement('button');
      el.type = 'button';
      el.className = 'map-cluster-marker';
      el.setAttribute(
        'aria-label',
        `${clusterDisplayLabel(cluster)} (${plural(cluster.count, 'photo')})`,
      );

      const img = document.createElement('img');
      img.src = mediaThumbUrl(cluster.cover_asset_id);
      img.alt = clusterDisplayLabel(cluster);
      img.loading = 'lazy';
      el.appendChild(img);

      const badge = document.createElement('span');
      badge.className = 'map-cluster-count';
      badge.textContent = String(cluster.count);
      el.appendChild(badge);

      el.addEventListener('click', () => {
        void handleSelectCluster(cluster);
      });

      const marker = new Marker({ element: el })
        .setLngLat([cluster.lon, cluster.lat])
        .addTo(map);
      markersRef.current.push(marker);
    }
  }, [webglAvailable, clusters, handleSelectCluster, mapInstanceVersion]);

  const stripAssetIds = useMemo(() => stripAssets.map((a) => a.id), [stripAssets]);
  const modeLabel = formatModePillLabel(config);
  const totalGeotagged = config?.total_geotagged ?? 0;

  useEffect(() => {
    if (!selectedCluster) return;
    const layer = pushEscapeLayer();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        setSelectedCluster(null);
        setStripAssets([]);
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [selectedCluster]);

  return (
    <div className="map-view-shell">
      <div className="map-status-pill" data-testid="map-status-pill">
        <span className="row" style={{ gap: 5 }}>
          {config?.tile_mode === 'online_openfreemap' ? (
            <Globe size={13} aria-hidden="true" />
          ) : (
            <MapPin size={13} aria-hidden="true" />
          )}
          <strong>{modeLabel}</strong>
        </span>
        <Badge tone={totalGeotagged > 0 ? 'ok' : 'muted'}>{totalGeotagged} geotagged</Badge>
        <button
          type="button"
          className="button"
          style={{ minHeight: 24, padding: '2px 8px', fontSize: 11 }}
          onClick={handleFitAll}
        >
          <Maximize2 size={11} />
          <span>Fit All</span>
        </button>
      </div>

      {error && (
        <div
          className="error-banner error-banner-dismissible"
          role="alert"
          style={{ margin: '58px 16px 0' }}
        >
          <span>{error}</span>
          <button type="button" className="button" onClick={() => void loadInitialMap()}>
            Retry
          </button>
        </div>
      )}

      {loading && !config ? (
        <div className="view-loading" role="status" style={{ paddingTop: 72 }}>
          Loading map &amp; GPS clusters…
        </div>
      ) : webglAvailable ? (
        <div ref={mapContainerRef} className="map-canvas" data-testid="map-canvas" />
      ) : (
        <div className="map-fallback-list" data-testid="map-fallback-list">
          {clusters.length === 0 ? (
            <EmptyState
              icon={<MapPin size={28} strokeWidth={1.3} />}
              title="No Geotagged Photos in View"
              description="Photos and videos with GPS metadata in your allowed folders will appear here."
            />
          ) : (
            clusters.map((cluster) => {
              const label = clusterDisplayLabel(cluster);
              const isSelected = selectedCluster?.id === cluster.id;
              return (
                <button
                  key={cluster.id}
                  type="button"
                  className="card card-body row"
                  style={{
                    justifyContent: 'space-between',
                    textAlign: 'left',
                    borderColor: isSelected ? '#e5e5e5' : undefined,
                  }}
                  onClick={() => void handleSelectCluster(cluster)}
                >
                  <div className="row" style={{ gap: 12, minWidth: 0 }}>
                    <img
                      src={mediaThumbUrl(cluster.cover_asset_id)}
                      alt={label}
                      width={44}
                      height={44}
                      style={{ borderRadius: 6, objectFit: 'cover', flexShrink: 0 }}
                    />
                    <div style={{ minWidth: 0 }}>
                      <div style={{ fontWeight: 600, fontSize: 13 }}>{label}</div>
                      <div className="view-subtitle" style={{ marginTop: 2 }}>
                        {cluster.lat.toFixed(3)}, {cluster.lon.toFixed(3)}
                      </div>
                    </div>
                  </div>
                  <Badge tone="info">{plural(cluster.count, 'photo')}</Badge>
                </button>
              );
            })
          )}
        </div>
      )}

      {selectedCluster && (
        <section
          className="map-photo-strip"
          aria-label={`Photos in ${clusterDisplayLabel(selectedCluster)}`}
        >
          <div className="row" style={{ justifyContent: 'space-between', gap: 8 }}>
            <div className="row" style={{ gap: 8 }}>
              <strong style={{ fontSize: 13 }}>{clusterDisplayLabel(selectedCluster)}</strong>
              <Badge>{plural(selectedCluster.count, 'photo')}</Badge>
            </div>
            <button
              type="button"
              className="icon-button"
              aria-label="Close cluster photos"
              onClick={() => {
                setSelectedCluster(null);
                setStripAssets([]);
              }}
            >
              <X size={15} />
            </button>
          </div>

          {loadingStrip ? (
            <div className="view-subtitle" role="status">
              Loading cluster photos…
            </div>
          ) : stripAssets.length === 0 ? (
            <div className="view-subtitle">No photos found in this cluster.</div>
          ) : (
            <div className="map-photo-strip-grid">
              {stripAssets.map((asset) => {
                const title = asset.title || formatDateTime(asset.capture_at_ms);
                return (
                  <button
                    key={asset.id}
                    type="button"
                    className="chat-hit-thumb"
                    style={{ width: 76, height: 76, flexShrink: 0 }}
                    aria-label={`Open ${title}`}
                    onClick={() => openLightbox(asset.id, stripAssetIds)}
                  >
                    <img src={mediaThumbUrl(asset.id)} alt={title} loading="lazy" />
                  </button>
                );
              })}
            </div>
          )}
        </section>
      )}
    </div>
  );
};
