<script>
  import { api } from './api.js';
  // maplibre-gl (~250 KB) is loaded on demand when the map is first drawn, so
  // the rest of the Stats view paints without pulling it.
  let maplibregl;

  let s = $state(null);
  let error = $state('');
  let mapEl;
  let paths = null;
  let mapDone = false;

  const STYLE = {
    version: 8,
    sources: { osm: { type: 'raster', tiles: ['https://a.tile.openstreetmap.org/{z}/{x}/{y}.png'], tileSize: 256, attribution: '© OpenStreetMap contributors' } },
    layers: [{ id: 'osm', type: 'raster', source: 'osm' }],
  };

  async function load() {
    error = '';
    try {
      s = await api.myStats();
      paths = await api.myPaths();
    } catch (e) { error = e.message; }
  }
  $effect(() => { load(); });

  // Draw the all-drives map once the container + path data are both ready.
  $effect(() => {
    if (mapDone || !mapEl || !paths || !paths.length) return;
    mapDone = true;
    drawMap();
  });

  async function drawMap() {
    if (!maplibregl) {
      [maplibregl] = await Promise.all([
        import('maplibre-gl').then((m) => m.default),
        import('maplibre-gl/dist/maplibre-gl.css'),
      ]);
    }
    const map = new maplibregl.Map({ container: mapEl, style: STYLE, center: [0, 0], zoom: 1 });
    const features = paths.map((p) => ({ type: 'Feature', properties: { autonomy: p.autonomy }, geometry: { type: 'LineString', coordinates: p.coords } }));
    let minx = 180, miny = 90, maxx = -180, maxy = -90;
    for (const p of paths) for (const [x, y] of p.coords) { minx = Math.min(minx, x); maxx = Math.max(maxx, x); miny = Math.min(miny, y); maxy = Math.max(maxy, y); }
    map.on('load', () => {
      map.addSource('drives', { type: 'geojson', data: { type: 'FeatureCollection', features } });
      map.addLayer({
        id: 'drives', type: 'line', source: 'drives',
        paint: {
          'line-width': 2.5, 'line-opacity': 0.7,
          'line-color': ['interpolate', ['linear'], ['get', 'autonomy'], 0, '#f85149', 50, '#d29922', 100, '#3fb950'],
        },
      });
      if (maxx >= minx) map.fitBounds([[minx, miny], [maxx, maxy]], { padding: 30, duration: 0 });
    });
  }

  const n0 = (x) => Math.round(x).toLocaleString();
  const n1 = (x) => (Math.round(x * 10) / 10).toLocaleString();
</script>

<div class="page">
  <div class="bar"><h2>Stats</h2><span class="muted">all-time</span></div>

  {#if error}
    <p class="error">{error}</p>
  {:else if !s}
    <p class="muted">Loading…</p>
  {:else if s.routes === 0}
    <p class="muted">No parsed drives yet.</p>
  {:else}
    <div class="hero">
      <div class="big">{n1(s.autonomy)}<span class="pct">%</span></div>
      <div class="muted">driven by openpilot · {n1(s.engaged_miles)} of {n1(s.miles)} mi</div>
    </div>

    <div class="grid">
      <div class="stat"><div class="v">{n0(s.miles)}</div><div class="k">miles driven</div></div>
      <div class="stat"><div class="v">{n1(s.drive_hours)}</div><div class="k">hours driving</div></div>
      <div class="stat"><div class="v">{n0(s.routes)}</div><div class="k">drives</div></div>
      <div class="stat"><div class="v">{n0(s.disengagements)}</div><div class="k">disengagements</div></div>
      <div class="stat"><div class="v">{n1(s.disengagements_per_100mi)}</div><div class="k">per 100 engaged mi</div></div>
      <div class="stat"><div class="v">{n0(s.avg_speed)}</div><div class="k">avg mph</div></div>
      <div class="stat"><div class="v">{n0(s.max_speed)}</div><div class="k">top mph</div></div>
      <div class="stat"><div class="v">{n0(s.hard_brake)}</div><div class="k">hard brakes</div></div>
      <div class="stat"><div class="v">{n0(s.hard_accel)}</div><div class="k">hard accels</div></div>
    </div>

    <div class="mapwrap">
      <div class="maplabel muted small">All drives — line color is autonomy (red → green)</div>
      <div class="map" bind:this={mapEl}></div>
    </div>
  {/if}
</div>

<style>
  .page { padding: 18px; max-width: 720px; margin: 0 auto; }
  .bar { display: flex; align-items: baseline; gap: 12px; margin-bottom: 18px; }
  h2 { margin: 0; }
  .hero { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; padding: 26px; text-align: center; margin-bottom: 16px; }
  .big { font-size: 64px; font-weight: 700; line-height: 1; color: var(--accent); }
  .big .pct { font-size: 28px; margin-left: 4px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); gap: 12px; }
  .stat { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; }
  .stat .v { font-size: 26px; font-weight: 700; }
  .stat .k { color: var(--muted); font-size: 13px; margin-top: 4px; }
  .mapwrap { margin-top: 16px; }
  .maplabel { margin-bottom: 6px; }
  .map { height: 360px; border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
</style>
