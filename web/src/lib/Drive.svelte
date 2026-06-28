<script>
  // The single-drive view: video playback (a stitched movie when ready, else HLS
  // with a separately-synced audio track over the muted full-res cams), a live GPS
  // marker + telemetry HUD driven off the video clock, and the optional model
  // overlay / top-down / graph panes (movable + resizable, layout persisted). All
  // {t,…} series are video-relative seconds; findNearest maps the clock to a sample.
  import { onMount, onDestroy } from 'svelte';
  // maplibre-gl (~250 KB) and hls.js (~150 KB) are loaded on demand in onMount so
  // the Login/Drives views don't pull them; assigned to these once imported.
  let maplibregl;
  let Hls;
  import { api, getToken } from './api.js';
  import { findNearest, copyText } from './format.js';
  import ManageData from './ManageData.svelte';
  import DriveGraph from './DriveGraph.svelte';
  import DriveModel from './DriveModel.svelte';
  import DriveOverlay from './DriveOverlay.svelte';

  let { route, onback, readonly = false } = $props();
  let showManage = $state(false);

  // Public sharing: toggle the drive's public flag and hand out a login-free link.
  let isPublic = $state(route.is_public ?? false);
  let shareCopied = $state(false);
  let shareErr = $state('');
  function shareLink() {
    return `${location.origin}/?share=${encodeURIComponent(route.fullname)}`;
  }
  async function copyShareLink() {
    if (await copyText(shareLink())) {
      shareCopied = true;
      setTimeout(() => (shareCopied = false), 1500);
    }
  }
  async function toggleShare() {
    shareErr = '';
    try {
      const r = await api.setRoutePublic(route.fullname, !isPublic);
      isPublic = r.is_public;
      if (isPublic) copyShareLink();
    } catch (e) {
      shareErr = e.message;
    }
  }

  let mapEl;
  let videoEl;
  let audioEl;
  let map;
  let marker;
  let hls;
  let audioHls;
  // Pre-built stitched movies (cam → {ready,bytes,duration}). When the current cam
  // has one, we play it directly (native audio, no HLS + audio-sync hack).
  let movies = $state({});
  let movieMode = $state(false);

  // Audio boost. The comma mic is quiet and the <video>/<audio> volume caps at
  // 100%; a Web Audio gain node lets us amplify past that. Both the movie's video
  // element and the HLS audio element feed one gain node → output.
  let volume = $state(Number(localStorage.getItem('hc_volume')) || 1); // 1 = 100%
  let audioCtx, gainNode, vidSrc, audSrc;
  function ensureAudioGraph() {
    if (audioCtx) { audioCtx.resume?.(); return; }
    try {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return;
      audioCtx = new AC();
      gainNode = audioCtx.createGain();
      gainNode.gain.value = volume;
      gainNode.connect(audioCtx.destination);
      // One source per element (createMediaElementSource is once-per-element);
      // the muted/idle one contributes silence, so this works in both modes.
      vidSrc = audioCtx.createMediaElementSource(videoEl);
      vidSrc.connect(gainNode);
      audSrc = audioCtx.createMediaElementSource(audioEl);
      audSrc.connect(gainNode);
      audioCtx.resume?.();
    } catch (e) {
      audioCtx = null; // unsupported → fall back to native (capped at 100%)
    }
  }
  function setVolume(v) {
    volume = v;
    localStorage.setItem('hc_volume', String(v));
    ensureAudioGraph();
    if (gainNode) gainNode.gain.value = v;
  }

  let coords = $state([]); // {t, lat, lng, speed}
  let telemetry = $state([]); // {t, speed, gear, lb, rb, brake, gas, steer, engaged}
  // Engage/disengage events derived from the continuous telemetry — only on a
  // real transition, so no duplicate or segment-boundary artifacts.
  // Why a disengagement happened, from the driver's inputs at the transition.
  function disengageReason(i) {
    // Look at the last engaged sample and the first disengaged one.
    for (const s of [telemetry[i], telemetry[i - 1]]) {
      if (!s) continue;
      if (s.gas) return 'gas';
      if (s.brake) return 'brake';
      if (s.steer_override) return 'steering';
    }
    return 'manual';
  }
  const reasonLabel = (r) =>
    ({ gas: 'gas pedal', brake: 'brake', steering: 'steering', manual: 'manual / off' })[r] ?? r;
  let engageEvents = $derived.by(() => {
    const out = [];
    let prev = false;
    for (let i = 0; i < telemetry.length; i++) {
      const e = !!telemetry[i].engaged;
      if (e !== prev) {
        out.push({ t: telemetry[i].t, engaged: e, reason: e ? null : disengageReason(i) });
        prev = e;
      }
    }
    return out;
  });
  let error = $state('');
  let curT = $state(0);
  let tnow = $state(null); // current telemetry sample
  let rate = $state(1);    // playback speed
  let cam = $state(localStorage.getItem('hc_cam') || 'qcamera');

  // Movable panes on a dense CSS grid. Each pane spans `cols` of GRID_COLS columns
  // and `rows` of fixed-height rows; dense auto-flow backfills gaps so a short pane
  // doesn't leave dead space. Drag a pane's header onto another to swap; drag the
  // bottom-right corner to resize (snapped to grid cells). Layout persists.
  const GRID_COLS = 4;
  const ROW_PX = 22; // grid-auto-rows height
  const GAP_PX = 8;
  const PANE_TITLES = { video: 'Video', map: 'Map', graph: 'Timeline', topdown: 'Top-down', events: 'Engagements' };
  const DEFAULT_PANES = [
    { id: 'video', cols: 2, rows: 18 },
    { id: 'map', cols: 2, rows: 18 },
    { id: 'graph', cols: 4, rows: 7 },
    { id: 'topdown', cols: 4, rows: 15 },
    { id: 'events', cols: 4, rows: 9 },
  ];
  function loadPanes() {
    try {
      const saved = JSON.parse(localStorage.getItem('hc_drive_panes') || 'null');
      if (Array.isArray(saved) && saved.length) {
        const order = saved.map((p) => p.id);
        const byId = Object.fromEntries(saved.map((p) => [p.id, p]));
        const merged = DEFAULT_PANES.map((d) => {
          const s = byId[d.id];
          if (!s) return { ...d };
          // Migrate the old {w:'half'|'full', h:px} format to {cols, rows}.
          const cols = s.cols ?? (s.w === 'full' ? 4 : 2);
          const rows = s.rows ?? Math.max(4, Math.round((s.h ?? 300) / (ROW_PX + GAP_PX)));
          return { id: d.id, cols, rows };
        });
        merged.sort((a, b) => {
          const ai = order.indexOf(a.id), bi = order.indexOf(b.id);
          return (ai < 0 ? 999 : ai) - (bi < 0 ? 999 : bi);
        });
        return merged;
      }
    } catch {}
    return structuredClone(DEFAULT_PANES);
  }
  let panes = $state(loadPanes());
  let panesEl;
  let dragId = $state(null);
  let overId = $state(null);
  // Top-down only appears when enabled; the others are always shown.
  let visiblePanes = $derived(panes.filter((p) => p.id !== 'topdown' || showModel));

  let saveTimer;
  function savePanes() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => localStorage.setItem('hc_drive_panes', JSON.stringify(panes)), 200);
  }
  function onDragStart(id, e) {
    dragId = id;
    e.dataTransfer.effectAllowed = 'move';
    try { e.dataTransfer.setData('text/plain', id); } catch {}
  }
  function onDrop(targetId) {
    overId = null;
    const from = panes.findIndex((p) => p.id === dragId);
    const to = panes.findIndex((p) => p.id === targetId);
    dragId = null;
    if (from < 0 || to < 0 || from === to) return;
    [panes[from], panes[to]] = [panes[to], panes[from]];
    panes = [...panes];
    savePanes();
  }
  function toggleWidth(id) {
    const p = panes.find((p) => p.id === id);
    if (p) { p.cols = p.cols >= GRID_COLS ? 2 : GRID_COLS; panes = [...panes]; savePanes(); }
  }
  function resetLayout() {
    panes = structuredClone(DEFAULT_PANES);
    localStorage.removeItem('hc_drive_panes');
  }
  // Drag the bottom-right corner to resize a pane (snapped to grid cells); dense
  // flow reflows the rest. The map's own ResizeObserver re-measures it.
  function startCornerResize(p, e) {
    e.preventDefault();
    e.stopPropagation();
    const rect = panesEl.getBoundingClientRect();
    const cellW = (rect.width - (GRID_COLS - 1) * GAP_PX) / GRID_COLS;
    const colStep = cellW + GAP_PX, rowStep = ROW_PX + GAP_PX;
    const sx = e.clientX, sy = e.clientY, sc = p.cols, sr = p.rows;
    const move = (ev) => {
      p.cols = Math.max(1, Math.min(GRID_COLS, sc + Math.round((ev.clientX - sx) / colStep)));
      p.rows = Math.max(4, sr + Math.round((ev.clientY - sy) / rowStep));
      panes = [...panes];
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      savePanes();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }
  // Keep maplibre sized to its (re-flowing) pane.
  function mapAutoResize(node) {
    let raf;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => map?.resize());
    });
    ro.observe(node);
    return { destroy() { ro.disconnect(); cancelAnimationFrame(raf); } };
  }

  // Which cameras this route actually has (from the route's max* fields).
  const cameras = [
    { id: 'qcamera', label: 'Road', has: true },
    { id: 'fcamera', label: 'Road HD', has: (route.maxcamera ?? -1) >= 0 },
    { id: 'dcamera', label: 'Driver', has: (route.maxdcamera ?? -1) >= 0 },
    { id: 'ecamera', label: 'Wide', has: (route.maxecamera ?? -1) >= 0 },
  ].filter((c) => c.has);

  const [dongle, ts] = route.fullname.split('|');
  const token = getToken();
  // Public (share-link) viewers have no token; public routes serve media without
  // a sig, so omit it rather than sending `?sig=null`.
  const sigQ = token ? `?sig=${token}` : '';

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
    return `/connectdata/${dongle}/${ts}/${n}/${file}${sigQ}`;
  }

  async function fetchJson(url) {
    const r = await fetch(url);
    if (!r.ok) return null;
    return r.json();
  }

  async function loadArtifacts() {
    const nums = route.segment_numbers?.length ? route.segment_numbers : [0];
    const coordChunks = await Promise.all(nums.map((n) => fetchJson(seg(n, 'coords.json'))));
    const telemChunks = await Promise.all(nums.map((n) => fetchJson(seg(n, 'telemetry.json'))));
    coords = coordChunks.filter(Boolean).flat();
    telemetry = telemChunks.filter(Boolean).flat();
  }

  // Top-down model view (modelV2 from the rlog) — lazy-loaded on first toggle.
  let showModel = $state(localStorage.getItem('hc_showModel') === '1');
  let modelFrames = $state([]);
  let modelRpy = $state([0, 0, 0]);
  let modelLoading = $state(false);
  let modelTried = false;
  async function loadModel() {
    if (modelTried) return;
    modelTried = true;
    modelLoading = true;
    const nums = route.segment_numbers?.length ? route.segment_numbers : [0];
    const chunks = await Promise.all(nums.map((n) => fetchJson(seg(n, 'model.json'))));
    modelFrames = chunks.filter(Boolean).flatMap((c) => c.frames || []);
    modelRpy = chunks.find(Boolean)?.rpy || [0, 0, 0];
    modelLoading = false;
  }
  function toggleModel() {
    showModel = !showModel;
    localStorage.setItem('hc_showModel', showModel ? '1' : '0');
    if (showModel) loadModel();
  }

  // On-video model overlay + per-camera calibration.
  const OVERLAY_CAMS = ['qcamera', 'fcamera', 'ecamera']; // road-facing
  const CAM_DIMS = { qcamera: [526, 330], fcamera: [1344, 760], ecamera: [1344, 760] };
  const CALIB_DEFAULTS = {
    qcamera: { fisheye: false, fx: 722.4, fy: 722.4, cx: 263, cy: 165, pitch: 0, yaw: 0, roll: 0, h: 1.2 },
    fcamera: { fisheye: false, fx: 1846, fy: 1846, cx: 672, cy: 380, pitch: 0, yaw: 0, roll: 0, h: 1.2 },
    ecamera: { fisheye: true, fx: 395, fy: 395, cx: 672, cy: 380, pitch: 0, yaw: 0, roll: 0, h: 1.2 },
  };
  let showOverlay = $state(localStorage.getItem('hc_showOverlay') === '1');
  let calibrating = $state(false);
  let calib = $state(null); // full per-camera object
  let calibMsg = $state('');
  let camCalib = $derived(calib?.[cam]);
  let camLabel = $derived(cameras.find((c) => c.id === cam)?.label ?? cam);
  let nw = $derived(CAM_DIMS[cam]?.[0] ?? 526);
  let nh = $derived(CAM_DIMS[cam]?.[1] ?? 330);
  let overlayOk = $derived(showOverlay && OVERLAY_CAMS.includes(cam) && modelFrames.length > 0 && !!camCalib);
  async function loadCalib() {
    if (calib) return;
    // Public viewers can't read the admin calibration; fall back to defaults
    // (good enough for the overlay) instead of a doomed request.
    if (readonly) { calib = structuredClone(CALIB_DEFAULTS); return; }
    try { calib = await api.camCalib(); } catch { calib = structuredClone(CALIB_DEFAULTS); }
  }
  async function toggleOverlay() {
    showOverlay = !showOverlay;
    localStorage.setItem('hc_showOverlay', showOverlay ? '1' : '0');
    if (showOverlay) { loadModel(); await loadCalib(); }
  }
  async function saveCalib() {
    try { await api.setCamCalib(calib); calibMsg = 'Calibration saved.'; setTimeout(() => (calibMsg = ''), 2500); }
    catch (e) { calibMsg = e.message; }
  }
  function resetCalib() {
    calib = { ...calib, [cam]: { ...CALIB_DEFAULTS[cam] } };
    calibMsg = `Reset ${cam} to defaults (not yet saved).`;
  }

  // Telemetry sample nearest the current playback time (binary search).
  const telemAt = (t) => findNearest(telemetry, t);

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

  function setRate(r) {
    rate = r;
    if (videoEl) videoEl.playbackRate = r;
    if (audioEl) audioEl.playbackRate = r;
  }

  // Move the marker to the GPS point nearest the video's current time.
  function syncMarker(t) {
    curT = t;
    tnow = telemAt(t);
    if (!marker || !coords.length) return;
    const c = findNearest(coords, t);
    marker.setLngLat([c.lng, c.lat]);
  }

  function loadVideo() {
    // A stitched movie exists for this camera → play it natively (audio muxed in),
    // no HLS, no separate audio track.
    if (movies[cam]?.ready) {
      movieMode = true;
      if (hls) { hls.destroy(); hls = null; }
      if (audioHls) { audioHls.destroy(); audioHls = null; }
      if (audioEl) { audioEl.pause(); audioEl.removeAttribute('src'); audioEl.load?.(); }
      videoEl.muted = false;
      videoEl.src = api.movieUrl(route.fullname, cam);
      videoEl.playbackRate = rate;
      return;
    }
    movieMode = false;
    videoEl.muted = true; // sound comes from the separate audio track
    videoEl.removeAttribute('src');
    const url = api.camM3u8(route.fullname, cam) + sigQ;
    if (hls) {
      hls.destroy();
      hls = null;
    }
    if (Hls.isSupported()) {
      // Full-res/driver segments are transcoded on first view (~seconds each),
      // so allow a generous fragment timeout and don't buffer many ahead (keeps
      // transcodes serialized, one at a time).
      hls = new Hls({ fragLoadingTimeOut: 90000, maxBufferLength: 30 });
      hls.loadSource(url);
      hls.attachMedia(videoEl);
    } else if (videoEl.canPlayType('application/vnd.apple.mpegurl')) {
      videoEl.src = url; // Safari native HLS
    } else {
      error = 'HLS playback not supported in this browser.';
    }
  }

  // Separate audio track (extracted from qcamera) played in sync with the
  // (muted) video — so the silent driver/full-res cameras have sound without
  // muxing audio into them. Continuous timeline matches the video.
  function loadAudio() {
    if (movieMode) return; // movie carries its own audio
    const url = api.camM3u8(route.fullname, 'audio') + sigQ;
    if (audioHls) { audioHls.destroy(); audioHls = null; }
    if (Hls.isSupported()) {
      audioHls = new Hls({ fragLoadingTimeOut: 90000 });
      audioHls.loadSource(url);
      audioHls.attachMedia(audioEl);
    } else if (audioEl.canPlayType('application/vnd.apple.mpegurl')) {
      audioEl.src = url;
    }
    audioEl.playbackRate = rate;
  }

  function wireAudioSync() {
    // No-op in movie mode — the movie has its own muxed audio track.
    const resync = () => { if (audioEl && !movieMode) audioEl.currentTime = videoEl.currentTime; };
    videoEl.addEventListener('play', () => { ensureAudioGraph(); if (movieMode) return; resync(); audioEl?.play().catch(() => {}); });
    videoEl.addEventListener('pause', () => { if (!movieMode) audioEl?.pause(); });
    videoEl.addEventListener('seeking', resync);
    videoEl.addEventListener('ratechange', () => { if (audioEl && !movieMode) audioEl.playbackRate = videoEl.playbackRate; });
    // Drift correction during playback.
    videoEl.addEventListener('timeupdate', () => {
      if (!movieMode && audioEl && !audioEl.paused && Math.abs(audioEl.currentTime - videoEl.currentTime) > 0.3) {
        audioEl.currentTime = videoEl.currentTime;
      }
    });
  }

  function switchCam(id) {
    const at = videoEl?.currentTime || 0;
    cam = id;
    localStorage.setItem('hc_cam', id);
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

  let mapReady = false;
  let drawn = false;
  // Draw exactly once, when BOTH the map style is loaded and coords are fetched
  // (either can finish first — attaching the load handler before any await
  // avoids missing the event when the style is cached on a 2nd drive).
  function maybeDraw() {
    if (drawn || !map || !mapReady || coords.length < 2) return;
    drawn = true;
    drawPath();
  }

  onMount(async () => {
    // A persisted camera the route doesn't have falls back to Road.
    if (!cameras.some((c) => c.id === cam)) cam = 'qcamera';
    // Restore persisted overlay/top-down state (their toggles didn't run).
    if (showModel || showOverlay) loadModel();
    if (showOverlay) loadCalib();
    // Pull the heavy map/video libs now (first use is below, all awaited).
    [maplibregl, Hls] = await Promise.all([
      import('maplibre-gl').then((m) => m.default),
      import('hls.js').then((m) => m.default),
      import('maplibre-gl/dist/maplibre-gl.css'),
    ]);
    map = new maplibregl.Map({ container: mapEl, style: STYLE, center: [0, 0], zoom: 1 });
    map.on('load', () => { mapReady = true; map.resize(); maybeDraw(); });
    videoEl.addEventListener('timeupdate', () => syncMarker(videoEl.currentTime));
    // A media (re)load resets playbackRate to 1; re-assert the selected speed.
    videoEl.addEventListener('loadedmetadata', () => {
      videoEl.playbackRate = rate;
      if (audioEl && !movieMode) audioEl.playbackRate = rate;
    });
    wireAudioSync();
    try {
      // Which cameras already have a stitched movie (decides the playback path).
      try {
        const r = await api.routeMovies(route.fullname);
        movies = Object.fromEntries(r.movies.map((m) => [m.cam, m]));
      } catch {}
      await loadArtifacts();
      maybeDraw();
      loadVideo();
      loadAudio();
    } catch (e) {
      error = e.message;
    }
  });

  onDestroy(() => {
    hls?.destroy();
    audioHls?.destroy();
    map?.remove();
  });
</script>

<div class="drive">
  <div class="bar">
    <button class="ghost" onclick={onback}>← Drives</button>
    <div class="title">{new Date(route.start_time_utc_millis).toLocaleString()}</div>
    <div class="muted">{route.length ? route.length.toFixed(1) + ' mi' : ''} · {route.platform || ''}</div>
    <div class="actions">
      {#if !readonly}
        {#if isPublic}
          <button class="ghost" onclick={copyShareLink} title="This drive is shared — anyone with the link can view it">
            {shareCopied ? '🔗 Link copied' : '🔗 Copy link'}
          </button>
          <button class="ghost" onclick={toggleShare} title="Make this drive private again">Stop sharing</button>
        {:else}
          <button class="ghost" onclick={toggleShare} title="Create a public, login-free link to this drive">Share</button>
        {/if}
        <button class="ghost" onclick={() => (showManage = true)}>Manage data</button>
      {/if}
      <button class="ghost" onclick={resetLayout} title="Reset the pane layout to default">Reset layout</button>
    </div>
  </div>
  {#if shareErr}<div class="muted pad error">{shareErr}</div>{/if}
  {#if isPublic && !readonly}<div class="muted pad small">Public link active — anyone with it can view this drive (no login).</div>{/if}

  {#if route.telem_miles > 0}
    <div class="statstrip">
      <span class="s"><b>{(route.autonomy ?? 0).toFixed(0)}%</b> openpilot</span>
      <span class="s"><b>{Math.round((route.drive_seconds ?? 0) / 60)}</b> min</span>
      <span class="s"><b>{(route.avg_speed ?? 0).toFixed(0)}</b> avg · <b>{(route.max_speed ?? 0).toFixed(0)}</b> max mph</span>
      <span class="s"><b>{route.disengage_count ?? 0}</b> disengage{(route.disengage_count ?? 0) === 1 ? '' : 's'}</span>
      {#if route.hard_brake_count}<span class="s">⚠ <b>{route.hard_brake_count}</b> hard brake{route.hard_brake_count === 1 ? '' : 's'}</span>{/if}
      {#if route.hard_accel_count}<span class="s">⚠ <b>{route.hard_accel_count}</b> hard accel{route.hard_accel_count === 1 ? '' : 's'}</span>{/if}
      {#if route.max_temp > 0}<span class="s">🌡 <b>{Math.round(route.max_temp)}</b>°C</span>{/if}
      {#if route.free_space >= 0}<span class="s"><b>{Math.round(route.free_space)}</b>% disk free</span>{/if}
    </div>
  {/if}

  {#if showManage}
    <ManageData
      {route}
      onclose={() => (showManage = false)}
      onchanged={() => { showManage = false; onback(); }} />
  {/if}

  {#if error}<div class="error pad">{error}</div>{/if}

  <div class="panes" bind:this={panesEl}>
    {#each visiblePanes as p (p.id)}
      <section
        class="pane"
        class:over={overId === p.id && dragId && dragId !== p.id}
        style="grid-column: span {p.cols}; grid-row: span {p.rows};"
        ondragover={(e) => { e.preventDefault(); overId = p.id; }}
        ondragleave={() => { if (overId === p.id) overId = null; }}
        ondrop={() => onDrop(p.id)}
      >
        <div
          class="pane-head"
          draggable="true"
          ondragstart={(e) => onDragStart(p.id, e)}
          ondragend={() => { dragId = null; overId = null; }}
          title="Drag to move this pane"
        >
          <span class="grip">⠿</span>
          <span class="pane-title">{PANE_TITLES[p.id]}</span>
          <button class="wtoggle" title="Half / full width" onclick={() => toggleWidth(p.id)}>
            {p.cols >= GRID_COLS ? '⤡' : '⤢'}
          </button>
        </div>

        <div class="pane-body">
          {#if p.id === 'video'}
            {#if cameras.length > 1}
              <div class="cams">
                {#each cameras as c}
                  <button class="ghost" class:active={c.id === cam} onclick={() => switchCam(c.id)}>{c.label}</button>
                {/each}
              </div>
            {/if}
            <div class="video-wrap">
              <video bind:this={videoEl} controls playsinline muted></video>
              {#if overlayOk}
                <DriveOverlay frames={modelFrames} rpy={modelRpy} {curT} calib={camCalib} fisheye={camCalib?.fisheye} {nw} {nh} />
              {/if}
              <audio bind:this={audioEl} style="display:none"></audio>
              {#if tnow}
                <div class="hud">
                  <div class="spd"><span class="n">{Math.round(tnow.speed)}</span><span class="u">mph</span></div>
                  <div class="chips">
                    <span class="chip">{tnow.gear?.toUpperCase() ?? '—'}</span>
                    {#if tnow.engaged}<span class="chip on">openpilot{tnow.lat === true && tnow.long === false ? ' · steer' : tnow.long === true && tnow.lat === false ? ' · cruise' : ''}</span>{/if}
                    {#if tnow.brake}<span class="chip brk">brake</span>{/if}
                    {#if tnow.dm_distracted}<span class="chip distr">distracted</span>{/if}
                    <span class="arrow" class:lit={tnow.lb}>◀</span>
                    <span class="arrow" class:lit={tnow.rb}>▶</span>
                  </div>
                </div>
              {/if}
            </div>
            <div class="ctrl">
              <span class="muted">t = {fmtT(curT)}</span>
              {#if movieMode}<span class="moviebadge" title="Playing the stitched HD movie with muxed audio">▶ HD movie</span>{/if}
              <button class="ghost rate" class:active={showModel} onclick={toggleModel} title="Top-down model view (needs full-res rlog)">Top-down</button>
              <button class="ghost rate" class:active={showOverlay} onclick={toggleOverlay} title="Model overlay on the road video">Overlay</button>
              {#if showOverlay && OVERLAY_CAMS.includes(cam) && !readonly}
                <button class="ghost rate" class:active={calibrating} onclick={() => (calibrating = !calibrating)}>Calibrate</button>
              {/if}
              <span class="rates">
                {#each [0.5, 1, 1.5, 2, 4, 8] as r}
                  <button class="ghost rate" class:active={rate === r} onclick={() => setRate(r)}>{r}×</button>
                {/each}
              </span>
              <label class="vol" title="Audio volume / boost (up to 400%)">
                <span class="vicon">🔊</span>
                <input type="range" min="0" max="4" step="0.1" value={volume} oninput={(e) => setVolume(+e.currentTarget.value)} />
                <span class="muted small vval">{Math.round(volume * 100)}%</span>
              </label>
            </div>
            {#if showOverlay && !OVERLAY_CAMS.includes(cam)}
              <div class="muted small pad">The overlay needs a road camera — switch to Road, Road HD, or Wide.</div>
            {/if}
            {#if calibrating && camCalib}
              <div class="calib">
                <div class="muted small">Calibrating <b>{camLabel}</b>{camCalib.fisheye ? ' · fisheye' : ''}</div>
                {#each [['fx', camCalib.fisheye ? 'focal' : 'focal x', 100, 3000, 1], ...(camCalib.fisheye ? [] : [['fy', 'focal y', 100, 3000, 1]]), ['cx', 'center x', 0, nw, 1], ['cy', 'center y', 0, nh, 1], ['pitch', 'pitch', -0.2, 0.2, 0.001], ['yaw', 'yaw', -0.2, 0.2, 0.001], ['roll', 'roll', -0.2, 0.2, 0.001], ['h', 'cam height m', 0, 2.5, 0.01]] as [k, label, min, max, step]}
                  <label class="crow">
                    <span>{label}</span>
                    <input type="range" {min} {max} {step} bind:value={calib[cam][k]} />
                    <span class="cval">{(+calib[cam][k]).toFixed(k === 'pitch' || k === 'yaw' || k === 'roll' ? 3 : k === 'h' ? 2 : 0)}</span>
                  </label>
                {/each}
                <div class="cactions">
                  <button onclick={saveCalib}>Save calibration</button>
                  <button class="ghost" onclick={resetCalib}>Reset {camLabel}</button>
                  {#if calibMsg}<span class="muted small">{calibMsg}</span>{/if}
                </div>
              </div>
            {/if}
          {:else if p.id === 'map'}
            <div class="map" bind:this={mapEl} use:mapAutoResize></div>
          {:else if p.id === 'graph'}
            <DriveGraph {telemetry} {curT} onseek={(t) => seek(t * 1000)} />
          {:else if p.id === 'topdown'}
            {#if modelLoading}
              <div class="muted small pad">Loading model…</div>
            {:else if modelFrames.length}
              <DriveModel frames={modelFrames} {curT} />
            {:else}
              <div class="muted small pad">No model data — pull full-res (rlog) for this drive first.</div>
            {/if}
          {:else if p.id === 'events'}
            <div class="events">
              {#if !engageEvents.length}
                <div class="muted small">No engagement events.</div>
              {:else}
                {#each engageEvents as e}
                  <button class="ev" onclick={() => seek(e.t * 1000)}>
                    <span class="badge" class:on={e.engaged}>{e.engaged ? 'engaged' : 'disengaged'}</span>
                    {#if e.reason}<span class="reason r-{e.reason}">{reasonLabel(e.reason)}</span>{/if}
                    <span class="muted small">{fmtT(e.t)}</span>
                  </button>
                {/each}
              {/if}
            </div>
          {/if}
        </div>
        <div class="resize-corner" title="Drag to resize" onpointerdown={(e) => startCornerResize(p, e)}></div>
      </section>
    {/each}
  </div>
</div>

<style>
  .drive { display: flex; flex-direction: column; height: 100%; }
  .bar {
    display: flex; flex-wrap: wrap; align-items: center; gap: 14px; padding: 10px 16px;
    border-bottom: 1px solid var(--border); background: var(--panel);
  }
  .bar .title { font-weight: 600; }
  .bar .actions { display: flex; align-items: center; gap: 14px; margin-left: auto; }
  /* On narrow screens drop the whole action group onto its own line under the
     drive description instead of spilling off the page. */
  @media (max-width: 640px) {
    .bar { gap: 8px; padding: 8px 12px; }
    .bar .title { font-size: 13px; }
    .bar .actions { width: 100%; margin-left: 0; gap: 8px; }
  }
  .pad { padding: 10px 16px; }
  .calib { padding: 8px 16px; display: grid; gap: 6px; border-bottom: 1px solid var(--border); }
  .crow { display: grid; grid-template-columns: 70px 1fr 46px; align-items: center; gap: 10px; font-size: 12px; color: var(--muted); }
  .crow input { width: 100%; }
  .crow .cval { text-align: right; color: var(--text); font-variant-numeric: tabular-nums; }
  .cactions { display: flex; gap: 10px; align-items: center; margin-top: 4px; }
  .statstrip { display: flex; flex-wrap: wrap; gap: 8px 18px; padding: 8px 16px;
    border-bottom: 1px solid var(--border); font-size: 13px; color: var(--muted); }
  .statstrip .s b { color: var(--text); }
  /* Movable panes on a dense grid: each spans cols×rows; dense flow fills gaps. */
  .panes { flex: 1; min-height: 0; overflow: auto; display: grid;
    grid-template-columns: repeat(4, 1fr); grid-auto-rows: 22px; grid-auto-flow: row dense;
    gap: 8px; padding: 8px; align-content: start; }
  .pane { position: relative; display: flex; flex-direction: column; min-width: 0; min-height: 0;
    background: var(--panel); border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
  .pane.over { outline: 2px dashed var(--accent); outline-offset: -2px; }
  /* Obvious corner grip: a tinted "fold" + diagonal gripper lines, big hit area. */
  .resize-corner { position: absolute; right: 0; bottom: 0; width: 30px; height: 30px;
    cursor: nwse-resize; touch-action: none; z-index: 3;
    background: linear-gradient(135deg, transparent 0 46%, var(--border) 46% 54%, var(--panel-2) 54%);
    /* Only the lower-right triangle is the hit area, so content beneath the rest
       of the square stays clickable. */
    clip-path: polygon(100% 0, 100% 100%, 0 100%); transition: background 0.1s; }
  .resize-corner::after { content: ''; position: absolute; right: 4px; bottom: 4px; width: 14px; height: 14px;
    background: repeating-linear-gradient(135deg, var(--muted) 0 2px, transparent 2px 4px);
    clip-path: polygon(100% 0, 100% 100%, 0 100%); }
  .resize-corner:hover { background: linear-gradient(135deg, transparent 0 46%, var(--accent) 46% 54%, var(--panel-2) 54%); }
  .resize-corner:hover::after { background: repeating-linear-gradient(135deg, var(--accent) 0 2px, transparent 2px 4px); }
  .pane-head { display: flex; align-items: center; gap: 8px; padding: 4px 8px; flex: none;
    background: var(--panel-2); border-bottom: 1px solid var(--border); cursor: grab; user-select: none; }
  .pane-head:active { cursor: grabbing; }
  .grip { color: var(--muted); font-size: 14px; line-height: 1; }
  .pane-title { font-size: 12px; font-weight: 600; color: var(--muted); flex: 1; }
  .wtoggle { background: none; border: 1px solid var(--border); border-radius: 6px; color: var(--muted);
    cursor: pointer; font-size: 12px; line-height: 1; padding: 2px 7px; }
  .wtoggle:hover { border-color: var(--accent); color: var(--accent); }
  .pane-body { flex: 1; min-height: 0; display: flex; flex-direction: column; overflow: auto; }
  .map { flex: 1; min-height: 0; }
  .cams { display: flex; gap: 6px; padding: 8px; border-bottom: 1px solid var(--border); flex: none; }
  .cams .active { border-color: var(--accent); color: var(--accent); }
  .video-wrap { position: relative; flex: 1; min-height: 0; background: #000; }
  video { width: 100%; height: 100%; background: #000; object-fit: contain; display: block; }
  .hud {
    position: absolute; top: 8px; left: 8px; right: 8px; display: flex;
    align-items: flex-start; justify-content: space-between; pointer-events: none;
    text-shadow: 0 1px 3px rgba(0,0,0,0.8);
  }
  .hud .spd { display: flex; align-items: baseline; gap: 4px; }
  .hud .spd .n { font-size: 30px; font-weight: 700; line-height: 1; }
  .hud .spd .u { font-size: 12px; color: #cfd6dd; }
  .hud .chips { display: flex; gap: 5px; align-items: center; flex-wrap: wrap; justify-content: flex-end; }
  .chip { font-size: 11px; padding: 2px 7px; border-radius: 999px; background: rgba(0,0,0,0.5); border: 1px solid rgba(255,255,255,0.25); }
  .chip.on { background: #2f81f7; border-color: #2f81f7; }
  .chip.brk { background: #f85149; border-color: #f85149; }
  .chip.distr { background: #a371f7; border-color: #a371f7; }
  .arrow { font-size: 16px; color: #444; }
  .arrow.lit { color: #3fb950; }
  .ctrl { display: flex; align-items: center; justify-content: space-between; gap: 8px 10px; flex-wrap: wrap; padding: 6px 12px; border-bottom: 1px solid var(--border); flex: none; }
  .moviebadge { font-size: 11px; color: #3fb950; border: 1px solid #2ea043; border-radius: 999px; padding: 1px 8px; }
  .vol { display: inline-flex; align-items: center; gap: 6px; }
  .vol input { width: 84px; }
  .vol .vicon { font-size: 13px; }
  .vol .vval { min-width: 34px; text-align: right; font-variant-numeric: tabular-nums; }
  .rates { display: flex; gap: 4px; }
  .rate { padding: 3px 8px; font-size: 12px; }
  .rate.active { border-color: var(--accent); color: var(--accent); }
  .clock { padding: 6px 12px; border-bottom: 1px solid var(--border); }
  .events { flex: 1; min-height: 0; overflow: auto; padding: 8px; }
  .ev {
    display: flex; align-items: center; justify-content: space-between; width: 100%;
    background: var(--panel-2); border: 1px solid var(--border); border-radius: 6px;
    padding: 8px 10px; margin-bottom: 6px; color: var(--text); cursor: pointer;
  }
  .badge { font-size: 12px; padding: 2px 8px; border-radius: 999px; background: #6e7681; color: #fff; }
  .badge.on { background: #3fb950; }
  .reason { font-size: 11px; padding: 1px 7px; border-radius: 999px; border: 1px solid var(--border); color: var(--muted); }
  .reason.r-gas, .reason.r-brake { color: #f0883e; border-color: #bb6a2a; }
  .reason.r-steering { color: #d29922; border-color: #9e7615; }
  .small { font-size: 12px; }
  @media (max-width: 800px) {
    .pane { grid-column: 1 / -1 !important; }
  }
</style>
