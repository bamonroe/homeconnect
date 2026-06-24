<script>
  import { onMount, onDestroy } from 'svelte';
  import maplibregl from 'maplibre-gl';
  import 'maplibre-gl/dist/maplibre-gl.css';
  import Hls from 'hls.js';
  import { api, getToken } from './api.js';

  let { route, onback } = $props();

  let mapEl;
  let videoEl;
  let map;
  let marker;
  let hls;

  let coords = $state([]); // {t, lat, lng, speed}
  let events = $state([]);
  let error = $state('');
  let curT = $state(0);
  let cam = $state('qcamera');

  // Which cameras this route actually has (from the route's max* fields).
  const cameras = [
    { id: 'qcamera', label: 'Road', has: true },
    { id: 'fcamera', label: 'Road HD', has: (route.maxcamera ?? -1) >= 0 },
    { id: 'dcamera', label: 'Driver', has: (route.maxdcamera ?? -1) >= 0 },
    { id: 'ecamera', label: 'Wide', has: (route.maxecamera ?? -1) >= 0 },
  ].filter((c) => c.has);

  const [dongle, ts] = route.fullname.split('|');
  const token = getToken();

  // Minimal OSM raster basemap (configurable later; fine for home use).
  const STYLE = {
    version: 8,
    sources: {
      osm: {
        type: 'raster',
        tiles: ['https://a.tile.openstreetmap.org/{z}/{x}/{y}.png'],
        tileSize: 256,
        attribution: '© OpenStreetMap contributors',
      },
    },
    layers: [{ id: 'osm', type: 'raster', source: 'osm' }],
  };

  function seg(n, file) {
    return `/connectdata/${dongle}/${ts}/${n}/${file}?sig=${token}`;
  }

  async function fetchJson(url) {
    const r = await fetch(url);
    if (!r.ok) return null;
    return r.json();
  }

  async function loadArtifacts() {
    const nums = route.segment_numbers?.length ? route.segment_numbers : [0];
    const coordChunks = await Promise.all(nums.map((n) => fetchJson(seg(n, 'coords.json'))));
    const eventChunks = await Promise.all(nums.map((n) => fetchJson(seg(n, 'events.json'))));
    coords = coordChunks.filter(Boolean).flat();
    events = eventChunks.filter(Boolean).flat();
  }

  function drawPath() {
    if (!map || coords.length < 2) return;
    const line = {
      type: 'Feature',
      geometry: { type: 'LineString', coordinates: coords.map((c) => [c.lng, c.lat]) },
    };
    if (map.getSource('path')) {
      map.getSource('path').setData(line);
    } else {
      map.addSource('path', { type: 'geojson', data: line });
      map.addLayer({
        id: 'path',
        type: 'line',
        source: 'path',
        paint: { 'line-color': '#2f81f7', 'line-width': 4 },
      });
    }
    const lons = coords.map((c) => c.lng);
    const lats = coords.map((c) => c.lat);
    map.fitBounds(
      [[Math.min(...lons), Math.min(...lats)], [Math.max(...lons), Math.max(...lats)]],
      { padding: 40, duration: 0 }
    );
    const first = coords[0];
    marker = new maplibregl.Marker({ color: '#f0b429' })
      .setLngLat([first.lng, first.lat])
      .addTo(map);
  }

  // Move the marker to the GPS point nearest the video's current time.
  function syncMarker(t) {
    curT = t;
    if (!marker || !coords.length) return;
    let lo = 0, hi = coords.length - 1, best = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (coords[mid].t <= t) { best = mid; lo = mid + 1; }
      else hi = mid - 1;
    }
    const c = coords[best];
    marker.setLngLat([c.lng, c.lat]);
  }

  function loadVideo() {
    const url = api.camM3u8(route.fullname, cam) + `?sig=${token}`;
    if (hls) {
      hls.destroy();
      hls = null;
    }
    if (Hls.isSupported()) {
      hls = new Hls();
      hls.loadSource(url);
      hls.attachMedia(videoEl);
    } else if (videoEl.canPlayType('application/vnd.apple.mpegurl')) {
      videoEl.src = url; // Safari native HLS
    } else {
      error = 'HLS playback not supported in this browser.';
    }
  }

  function switchCam(id) {
    const at = videoEl?.currentTime || 0;
    cam = id;
    loadVideo();
    // Restore position after the new source loads.
    const restore = () => {
      try { videoEl.currentTime = at; } catch {}
      videoEl.removeEventListener('loadedmetadata', restore);
    };
    videoEl.addEventListener('loadedmetadata', restore);
  }

  function seek(ms) {
    if (videoEl) {
      videoEl.currentTime = ms / 1000;
      videoEl.play?.();
    }
  }

  function fmtT(s) {
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${String(sec).padStart(2, '0')}`;
  }

  onMount(async () => {
    map = new maplibregl.Map({ container: mapEl, style: STYLE, center: [0, 0], zoom: 1 });
    try {
      await loadArtifacts();
      map.on('load', drawPath);
      if (map.loaded()) drawPath();
      videoEl.addEventListener('timeupdate', () => syncMarker(videoEl.currentTime));
      loadVideo();
    } catch (e) {
      error = e.message;
    }
  });

  onDestroy(() => {
    hls?.destroy();
    map?.remove();
  });
</script>

<div class="drive">
  <div class="bar">
    <button class="ghost" onclick={onback}>← Drives</button>
    <div class="title">{new Date(route.start_time_utc_millis).toLocaleString()}</div>
    <div class="muted">{route.length ? route.length.toFixed(1) + ' mi' : ''} · {route.platform || ''}</div>
  </div>

  {#if error}<div class="error pad">{error}</div>{/if}

  <div class="grid">
    <div class="map" bind:this={mapEl}></div>
    <div class="side">
      {#if cameras.length > 1}
        <div class="cams">
          {#each cameras as c}
            <button class="ghost" class:active={c.id === cam} onclick={() => switchCam(c.id)}>{c.label}</button>
          {/each}
        </div>
      {/if}
      <video bind:this={videoEl} controls playsinline muted></video>
      <div class="clock muted">t = {fmtT(curT)}</div>
      <div class="events">
        <div class="ev-head">Events</div>
        {#if !events.length}
          <div class="muted small">No engagement events.</div>
        {:else}
          {#each events as e}
            <button class="ev" onclick={() => seek(e.route_offset_millis)}>
              <span class="badge" class:on={e.data?.enabled}>{e.data?.enabled ? 'engaged' : 'disengaged'}</span>
              <span class="muted small">{fmtT((e.route_offset_millis || 0) / 1000)}</span>
            </button>
          {/each}
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .drive { display: flex; flex-direction: column; height: 100%; }
  .bar {
    display: flex; align-items: center; gap: 14px; padding: 10px 16px;
    border-bottom: 1px solid var(--border); background: var(--panel);
  }
  .bar .title { font-weight: 600; }
  .pad { padding: 10px 16px; }
  .grid { flex: 1; min-height: 0; display: grid; grid-template-columns: 1fr 380px; }
  .map { height: 100%; }
  .side { border-left: 1px solid var(--border); display: flex; flex-direction: column; min-height: 0; }
  .cams { display: flex; gap: 6px; padding: 8px; border-bottom: 1px solid var(--border); }
  .cams .active { border-color: var(--accent); color: var(--accent); }
  video { width: 100%; background: #000; aspect-ratio: 16/9; }
  .clock { padding: 6px 12px; border-bottom: 1px solid var(--border); }
  .events { overflow: auto; padding: 8px; }
  .ev-head { font-weight: 600; margin: 4px 6px 8px; }
  .ev {
    display: flex; align-items: center; justify-content: space-between; width: 100%;
    background: var(--panel-2); border: 1px solid var(--border); border-radius: 6px;
    padding: 8px 10px; margin-bottom: 6px; color: var(--text); cursor: pointer;
  }
  .badge { font-size: 12px; padding: 2px 8px; border-radius: 999px; background: #6e7681; color: #fff; }
  .badge.on { background: #3fb950; }
  .small { font-size: 12px; }
  @media (max-width: 800px) {
    .grid { grid-template-columns: 1fr; grid-template-rows: 1fr auto; }
    .side { border-left: 0; border-top: 1px solid var(--border); }
  }
</style>
