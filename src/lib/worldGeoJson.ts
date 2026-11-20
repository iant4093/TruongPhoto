// Compact simplified Natural Earth 110m world land polygons and 30-degree graticule
// for instant zero-network offline MapLibre GL JS basemap rendering.

export interface WorldLandFeature {
  type: 'Feature';
  properties: {
    name: string;
    region: string;
  };
  geometry: {
    type: 'Polygon';
    coordinates: [number, number][][];
  };
}

export interface WorldLandFeatureCollection {
  type: 'FeatureCollection';
  features: WorldLandFeature[];
}

export interface WorldGraticuleFeature {
  type: 'Feature';
  properties: {
    kind: 'meridian' | 'parallel';
    deg: number;
  };
  geometry: {
    type: 'LineString';
    coordinates: [number, number][];
  };
}

export interface WorldGraticuleFeatureCollection {
  type: 'FeatureCollection';
  features: WorldGraticuleFeature[];
}

function closeRing(points: [number, number][]): [number, number][][] {
  if (points.length === 0) return [[]];
  const first = points[0];
  const last = points[points.length - 1];
  if (first[0] === last[0] && first[1] === last[1]) {
    return [points];
  }
  return [[...points, [first[0], first[1]]]];
}

export const WORLD_LAND_GEOJSON: WorldLandFeatureCollection = {
  type: 'FeatureCollection',
  features: [
    {
      type: 'Feature',
      properties: { name: 'North America', region: 'Americas' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-168.0, 65.5], [-162.0, 70.5], [-141.0, 69.8], [-130.0, 70.2], [-115.0, 69.5],
          [-95.0, 71.0], [-82.0, 69.5], [-78.0, 63.0], [-83.0, 62.0], [-92.0, 58.0],
          [-82.0, 52.0], [-79.0, 54.5], [-77.0, 61.5], [-66.0, 60.5], [-56.0, 52.0],
          [-60.0, 46.5], [-67.0, 45.0], [-70.5, 42.0], [-75.5, 36.0], [-81.0, 31.0],
          [-80.2, 25.3], [-82.5, 27.5], [-85.0, 30.0], [-90.0, 29.0], [-97.2, 26.0],
          [-96.5, 19.5], [-91.0, 18.5], [-89.5, 21.3], [-87.0, 21.4], [-88.3, 16.0],
          [-83.5, 15.0], [-83.5, 10.5], [-78.0, 8.5], [-79.5, 9.0], [-85.5, 10.5],
          [-92.2, 14.5], [-100.0, 16.8], [-105.5, 20.5], [-108.5, 25.5], [-112.5, 30.5],
          [-110.0, 23.0], [-112.0, 25.0], [-117.1, 32.5], [-120.5, 34.5], [-122.5, 37.8],
          [-124.5, 42.5], [-124.5, 48.5], [-130.5, 54.5], [-136.0, 58.5], [-146.0, 60.5],
          [-152.0, 59.5], [-161.5, 55.5], [-166.0, 59.0], [-162.0, 63.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Cuba', region: 'Americas' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-84.9, 21.8], [-82.0, 23.1], [-78.0, 22.1], [-74.2, 20.2],
          [-77.5, 19.8], [-81.5, 21.8],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Hispaniola', region: 'Americas' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-74.4, 18.5], [-72.8, 19.9], [-68.4, 18.6], [-70.2, 18.2], [-73.5, 18.1],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'South America', region: 'Americas' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-78.0, 8.5], [-75.5, 10.8], [-71.5, 12.2], [-66.5, 10.6], [-61.5, 10.2],
          [-56.0, 6.0], [-50.0, 1.5], [-46.0, -1.5], [-35.0, -5.8], [-34.8, -8.5],
          [-38.5, -13.0], [-40.5, -21.0], [-44.5, -23.2], [-48.5, -27.5], [-53.5, -33.8],
          [-58.0, -35.0], [-62.0, -39.5], [-65.0, -43.5], [-67.5, -48.5], [-68.5, -54.8],
          [-71.5, -54.5], [-75.0, -51.5], [-74.0, -45.5], [-73.5, -39.5], [-71.6, -33.0],
          [-70.5, -23.5], [-70.3, -18.4], [-77.1, -12.1], [-81.3, -4.7], [-80.0, -2.0],
          [-80.8, 0.5], [-77.5, 4.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Greenland', region: 'Americas' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-44.0, 60.0], [-52.0, 64.5], [-53.0, 70.0], [-60.0, 75.5], [-72.0, 77.5],
          [-62.0, 81.5], [-35.0, 83.2], [-20.0, 81.0], [-18.0, 74.5], [-23.0, 70.0],
          [-34.0, 67.5], [-41.0, 63.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Iceland', region: 'Europe' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-24.2, 65.5], [-21.5, 66.5], [-14.0, 66.0], [-13.8, 64.3],
          [-19.5, 63.4], [-22.8, 63.9],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Great Britain', region: 'Europe' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-5.5, 50.1], [-4.5, 53.3], [-3.0, 55.0], [-5.8, 57.5], [-3.2, 58.6],
          [-1.8, 57.5], [-1.5, 55.0], [1.7, 52.6], [1.4, 51.1], [-2.5, 50.6],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Ireland', region: 'Europe' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-10.2, 51.8], [-9.8, 54.3], [-6.2, 55.2], [-5.6, 54.2],
          [-6.2, 52.2], [-8.5, 51.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Europe', region: 'Europe' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-9.3, 38.7], [-9.2, 43.0], [-1.8, 43.4], [-4.5, 48.3], [1.5, 50.9],
          [4.8, 52.8], [8.5, 54.5], [8.2, 57.1], [10.6, 57.7], [12.5, 55.8],
          [11.0, 58.5], [5.2, 60.4], [8.0, 63.5], [15.0, 68.0], [24.0, 71.0],
          [31.0, 70.0], [42.0, 67.5], [59.0, 69.5], [60.0, 60.0], [54.0, 52.0],
          [48.0, 46.5], [40.0, 43.5], [35.0, 45.2], [29.0, 41.2], [26.2, 40.5],
          [23.7, 37.9], [20.5, 39.5], [18.4, 40.0], [16.0, 38.0], [12.5, 41.8],
          [9.0, 44.4], [5.0, 43.3], [3.2, 42.4], [0.0, 38.5], [-5.5, 36.0],
          [-8.8, 37.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Africa', region: 'Africa' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-5.6, 35.8], [3.0, 36.8], [11.0, 37.2], [11.5, 33.2], [20.0, 32.5],
          [32.5, 31.3], [34.5, 28.0], [39.0, 21.5], [43.3, 12.6], [51.3, 11.9],
          [48.5, 5.0], [41.5, -1.5], [40.5, -10.5], [35.0, -19.5], [33.0, -26.0],
          [28.0, -33.0], [20.0, -34.8], [18.3, -34.2], [15.0, -26.0], [12.0, -16.0],
          [13.2, -8.8], [9.4, 0.5], [8.5, 4.5], [3.5, 6.4], [-4.0, 5.2],
          [-8.0, 4.4], [-13.5, 8.5], [-17.5, 14.7], [-16.5, 21.0], [-13.2, 27.5],
          [-9.5, 32.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Madagascar', region: 'Africa' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [49.3, -12.0], [50.4, -15.5], [47.6, -24.5], [44.2, -25.5],
          [43.5, -21.0], [45.5, -16.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Asia', region: 'Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [26.5, 40.0], [36.0, 41.5], [41.5, 41.5], [40.0, 43.5], [48.0, 46.5],
          [54.0, 52.0], [60.0, 60.0], [59.0, 69.5], [73.0, 72.8], [95.0, 76.5],
          [115.0, 73.5], [140.0, 72.5], [170.0, 69.5], [178.0, 66.5], [163.0, 60.0],
          [156.5, 51.0], [142.0, 54.0], [135.5, 44.0], [129.0, 35.1], [126.5, 37.5],
          [122.0, 39.5], [121.5, 31.2], [119.5, 25.0], [114.2, 22.3], [108.5, 21.5],
          [109.2, 12.0], [105.0, 8.6], [100.5, 13.5], [103.8, 1.3], [98.5, 8.0],
          [96.0, 16.5], [91.8, 22.0], [87.0, 21.5], [80.2, 13.0], [77.5, 8.1],
          [72.8, 19.0], [68.5, 23.5], [62.0, 25.2], [56.5, 26.2], [51.0, 26.0],
          [56.2, 22.5], [59.8, 22.5], [53.0, 16.8], [43.5, 12.7], [39.0, 21.5],
          [34.8, 29.5], [32.5, 31.3], [35.5, 36.8], [27.0, 38.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Sri Lanka', region: 'Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [80.0, 9.8], [81.8, 7.6], [80.6, 5.9], [79.7, 7.2],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Taiwan', region: 'Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [121.5, 25.2], [121.9, 24.5], [120.8, 22.0], [120.1, 23.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Japan', region: 'Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [130.5, 31.4], [131.5, 33.8], [135.5, 34.5], [139.8, 35.5], [141.5, 38.5],
          [141.0, 41.3], [145.5, 43.3], [141.8, 45.4], [139.8, 42.5], [139.5, 38.0],
          [136.5, 36.8], [132.5, 34.8], [129.8, 33.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Sumatra', region: 'Southeast Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [95.3, 5.5], [99.5, 3.0], [104.5, -1.5], [105.8, -5.8],
          [102.5, -4.0], [97.5, 1.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Java', region: 'Southeast Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [105.5, -6.2], [110.5, -6.8], [114.5, -7.8], [114.0, -8.6],
          [108.0, -7.8], [105.8, -7.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Borneo', region: 'Southeast Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [109.2, 1.6], [115.0, 5.0], [117.5, 6.9], [118.8, 4.2],
          [117.5, -1.0], [114.5, -3.5], [110.0, -3.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Sulawesi', region: 'Southeast Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [119.5, 1.2], [125.0, 1.5], [123.5, -1.5], [122.8, -5.0],
          [119.5, -5.5], [118.9, -2.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Philippines', region: 'Southeast Asia' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [120.2, 18.5], [122.4, 16.5], [124.2, 13.2], [126.5, 8.0],
          [125.2, 5.6], [122.0, 7.0], [122.5, 11.5], [120.0, 14.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'New Guinea', region: 'Oceania' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [131.0, -1.5], [136.0, -2.0], [141.0, -2.6], [147.0, -6.5],
          [150.5, -10.5], [143.5, -8.5], [138.0, -8.2], [133.0, -4.2],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Australia', region: 'Oceania' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [114.0, -22.0], [122.2, -17.5], [130.8, -12.4], [136.8, -12.3],
          [136.5, -15.5], [141.5, -15.5], [142.5, -10.7], [146.0, -17.5],
          [150.5, -22.5], [153.6, -28.2], [151.2, -33.9], [149.9, -37.5],
          [144.9, -38.4], [138.6, -35.5], [135.5, -34.8], [129.0, -31.8],
          [121.9, -33.9], [115.1, -34.3], [115.8, -31.9], [113.5, -26.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Tasmania', region: 'Oceania' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [144.7, -40.8], [148.2, -40.8], [147.9, -43.2], [145.3, -43.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'New Zealand North Island', region: 'Oceania' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [172.7, -34.5], [174.8, -36.8], [178.5, -37.7], [177.8, -39.5],
          [174.8, -41.3], [173.8, -39.0],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'New Zealand South Island', region: 'Oceania' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [172.5, -40.6], [174.2, -41.7], [171.2, -44.5], [168.4, -46.6],
          [166.5, -45.8], [170.5, -42.5],
        ]),
      },
    },
    {
      type: 'Feature',
      properties: { name: 'Antarctica', region: 'Antarctica' },
      geometry: {
        type: 'Polygon',
        coordinates: closeRing([
          [-180.0, -84.0], [-180.0, -77.5], [-155.0, -77.5], [-135.0, -74.0],
          [-100.0, -72.5], [-75.0, -72.0], [-62.0, -64.5], [-55.0, -64.0],
          [-58.0, -74.0], [-40.0, -77.5], [-15.0, -72.0], [10.0, -70.0],
          [40.0, -68.0], [70.0, -68.5], [95.0, -66.5], [130.0, -66.5],
          [165.0, -71.0], [170.0, -77.5], [180.0, -77.5], [180.0, -84.0],
        ]),
      },
    },
  ],
};

function buildGraticule(): WorldGraticuleFeatureCollection {
  const features: WorldGraticuleFeature[] = [];
  const meridians = [-180, -150, -120, -90, -60, -30, 0, 30, 60, 90, 120, 150, 180];
  const parallels = [-60, -30, 0, 30, 60];

  for (const lon of meridians) {
    const coords: [number, number][] = [];
    for (let lat = -84; lat <= 84; lat += 12) {
      coords.push([lon, lat]);
    }
    features.push({
      type: 'Feature',
      properties: { kind: 'meridian', deg: lon },
      geometry: { type: 'LineString', coordinates: coords },
    });
  }

  for (const lat of parallels) {
    const coords: [number, number][] = [];
    for (let lon = -180; lon <= 180; lon += 15) {
      coords.push([lon, lat]);
    }
    features.push({
      type: 'Feature',
      properties: { kind: 'parallel', deg: lat },
      geometry: { type: 'LineString', coordinates: coords },
    });
  }

  return {
    type: 'FeatureCollection',
    features,
  };
}

export const WORLD_GRATICULE_GEOJSON: WorldGraticuleFeatureCollection = buildGraticule();
