<script>
  import { onMount, onDestroy } from 'svelte';
  import maplibregl from 'maplibre-gl';
  import 'maplibre-gl/dist/maplibre-gl.css';
  import Hls from 'hls.js';
  import { api, getToken } from './api.js';
  import ManageData from './ManageData.svelte';
  import DriveGraph from './DriveGraph.svelte';
  import DriveModel from './DriveModel.svelte';
  import DriveOverlay from './DriveOverlay.svelte';

  let { route, onback } = $props();
  let showManage = $state(false);
  let pulling = $state(false);
  let pullMsg = $state('');

  let mapEl;
  let videoEl;
  let audioEl;
  let map;
  let marker;
  let hls;
  let audioHls;

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
  let cam = $state('qcamera');

  // Resizable panes (persisted). rightW = width of the video/events column;
  // videoH = height of the video within that column.
  let gridEl, videoWrapEl;
  let rightW = $state(Number(localStorage.getItem('hc_rightW')) || 620);
  let videoH = $state(Number(localStorage.getItem('hc_videoH')) || 400);

  function startColResize(e) {
    e.preventDefault();
    const rect = gridEl.getBoundingClientRect();
    const move = (ev) => {
      rightW = Math.max(280, Math.min(rect.width - 220, rect.right - ev.clientX));
      map?.resize();
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      localStorage.setItem('hc_rightW', rightW);
      map?.resize();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function startRowResize(e) {
    e.preventDefault();
    const move = (ev) => {
      const top = videoWrapEl.getBoundingClientRect().top;
      videoH = Math.max(120, ev.clientY - top);
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      localStorage.setItem('hc_videoH', videoH);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
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
    const telemChunks = await Promise.all(nums.map((n) => fetchJson(seg(n, 'telemetry.json'))));
    coords = coordChunks.filter(Boolean).flat();
    telemetry = telemChunks.filter(Boolean).flat();
  }

  // Top-down model view (modelV2 from the rlog) — lazy-loaded on first toggle.
  let showModel = $state(false);
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
    if (showModel) loadModel();
  }

  // On-video model overlay + its calibration.
  let showOverlay = $state(false);
  let calibrating = $state(false);
  let calib = $state(null);
  let calibMsg = $state('');
  async function toggleOverlay() {
    showOverlay = !showOverlay;
    if (showOverlay) {
      loadModel();
      if (!calib) { try { calib = { ...CALIB_DEFAULTS, ...(await api.camCalib()) }; } catch { calib = { ...CALIB_DEFAULTS }; } }
    }
  }
  const CALIB_DEFAULTS = { fx: 722.4, fy: 722.4, cx: 263, cy: 165, pitch: 0, yaw: 0, roll: 0, h: 1.2 };
  async function saveCalib() {
    try { await api.setCamCalib(calib); calibMsg = 'Calibration saved.'; setTimeout(() => (calibMsg = ''), 2500); }
    catch (e) { calibMsg = e.message; }
  }
  function resetCalib() {
    calib = { ...CALIB_DEFAULTS };
    calibMsg = 'Reset to defaults (not yet saved).';
  }

  // Telemetry sample nearest the current playback time (binary search).
  function telemAt(t) {
    if (!telemetry.length) return null;
    let lo = 0, hi = telemetry.length - 1, best = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (telemetry[mid].t <= t) { best = mid; lo = mid + 1; }
      else hi = mid - 1;
    }
    return telemetry[best];
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
    const url = api.camM3u8(route.fullname, 'audio') + `?sig=${token}`;
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
    const resync = () => { if (audioEl) audioEl.currentTime = videoEl.currentTime; };
    videoEl.addEventListener('play', () => { resync(); audioEl?.play().catch(() => {}); });
    videoEl.addEventListener('pause', () => audioEl?.pause());
    videoEl.addEventListener('seeking', resync);
    videoEl.addEventListener('ratechange', () => { if (audioEl) audioEl.playbackRate = videoEl.playbackRate; });
    // Drift correction during playback.
    videoEl.addEventListener('timeupdate', () => {
      if (audioEl && !audioEl.paused && Math.abs(audioEl.currentTime - videoEl.currentTime) > 0.3) {
        audioEl.currentTime = videoEl.currentTime;
      }
    });
  }

  // Pull this drive's data per the default sync settings (catches anything not
  // yet synced for this route). Granular per-type pulls live in Manage data.
  async function syncDrive() {
    if (pulling) return;
    pulling = true;
    pullMsg = '';
    try {
      const s = await api.sync(dongle, { route: ts });
      pullMsg = s.online === false
        ? 'Device is offline — it’ll sync when it reconnects.'
        : 'Sync queued — progress shows up top; reopen the drive once it lands.';
    } catch (e) {
      pullMsg = `Sync failed: ${e.message}`;
    } finally {
      pulling = false;
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
    map = new maplibregl.Map({ container: mapEl, style: STYLE, center: [0, 0], zoom: 1 });
    map.on('load', () => { mapReady = true; map.resize(); maybeDraw(); });
    videoEl.addEventListener('timeupdate', () => syncMarker(videoEl.currentTime));
    wireAudioSync();
    try {
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
    <button class="ghost pullfull" onclick={syncDrive} disabled={pulling}>
      {pulling ? 'Working…' : 'Sync'}
    </button>
    <button class="ghost manage" onclick={() => (showManage = true)}>Manage data</button>
  </div>

  {#if pullMsg}<div class="muted pad">{pullMsg}</div>{/if}

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

  <div class="grid" bind:this={gridEl}>
    <div class="map" bind:this={mapEl}></div>
    <div class="col-resizer" onpointerdown={startColResize} title="Drag to resize"></div>
    <div class="side" style="width:{rightW}px">
      {#if cameras.length > 1}
        <div class="cams">
          {#each cameras as c}
            <button class="ghost" class:active={c.id === cam} onclick={() => switchCam(c.id)}>{c.label}</button>
          {/each}
        </div>
      {/if}
      <div class="video-wrap" bind:this={videoWrapEl} style="height:{videoH}px">
        <video bind:this={videoEl} controls playsinline muted></video>
        {#if showOverlay && cam === 'qcamera' && modelFrames.length}
          <DriveOverlay frames={modelFrames} rpy={modelRpy} {curT} {calib} />
        {/if}
        <audio bind:this={audioEl} style="display:none"></audio>
        {#if tnow}
          <div class="hud">
            <div class="spd"><span class="n">{Math.round(tnow.speed)}</span><span class="u">mph</span></div>
            <div class="chips">
              <span class="chip">{tnow.gear?.toUpperCase() ?? '—'}</span>
              {#if tnow.engaged}<span class="chip on">openpilot</span>{/if}
              {#if tnow.brake}<span class="chip brk">brake</span>{/if}
              <span class="arrow" class:lit={tnow.lb}>◀</span>
              <span class="arrow" class:lit={tnow.rb}>▶</span>
            </div>
          </div>
        {/if}
      </div>
      <div class="ctrl">
        <span class="muted">t = {fmtT(curT)}</span>
        <button class="ghost rate" class:active={showModel} onclick={toggleModel} title="Top-down model view (needs full-res rlog)">Top-down</button>
        <button class="ghost rate" class:active={showOverlay} onclick={toggleOverlay} title="Model overlay on the road video (Road cam)">Overlay</button>
        {#if showOverlay && cam === 'qcamera'}
          <button class="ghost rate" class:active={calibrating} onclick={() => (calibrating = !calibrating)}>Calibrate</button>
        {/if}
        <span class="rates">
          {#each [0.5, 1, 1.5, 2, 4, 8] as r}
            <button class="ghost rate" class:active={rate === r} onclick={() => setRate(r)}>{r}×</button>
          {/each}
        </span>
      </div>
      {#if showOverlay && cam !== 'qcamera'}
        <div class="muted small pad">The overlay is calibrated for the Road (qcamera) view — switch to Road.</div>
      {/if}
      {#if calibrating && calib}
        <div class="calib">
          {#each [['fx', 'focal x', 300, 1400, 1], ['fy', 'focal y', 300, 1400, 1], ['cx', 'center x', 0, 526, 1], ['cy', 'center y', 0, 330, 1], ['pitch', 'pitch', -0.15, 0.15, 0.001], ['yaw', 'yaw', -0.15, 0.15, 0.001], ['h', 'cam height m', 0, 2.5, 0.01]] as [k, label, min, max, step]}
            <label class="crow">
              <span>{label}</span>
              <input type="range" {min} {max} {step} bind:value={calib[k]} />
              <span class="cval">{(+calib[k]).toFixed(k === 'pitch' || k === 'yaw' ? 3 : k === 'h' ? 2 : 0)}</span>
            </label>
          {/each}
          <div class="cactions">
            <button onclick={saveCalib}>Save calibration</button>
            <button class="ghost" onclick={resetCalib}>Reset</button>
            {#if calibMsg}<span class="muted small">{calibMsg}</span>{/if}
          </div>
        </div>
      {/if}
      <DriveGraph {telemetry} {curT} onseek={(t) => seek(t * 1000)} />
      {#if showModel}
        {#if modelLoading}
          <div class="muted small pad">Loading model…</div>
        {:else if modelFrames.length}
          <DriveModel frames={modelFrames} {curT} />
        {:else}
          <div class="muted small pad">No model data — pull full-res (rlog) for this drive first.</div>
        {/if}
      {/if}
      <div class="row-resizer" onpointerdown={startRowResize} title="Drag to resize"></div>
      <div class="events">
        <div class="ev-head">Engagements</div>
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
  .bar .pullfull { margin-left: auto; }
  .bar .manage { margin-left: auto; }
  .pad { padding: 10px 16px; }
  .calib { padding: 8px 16px; display: grid; gap: 6px; border-bottom: 1px solid var(--border); }
  .crow { display: grid; grid-template-columns: 70px 1fr 46px; align-items: center; gap: 10px; font-size: 12px; color: var(--muted); }
  .crow input { width: 100%; }
  .crow .cval { text-align: right; color: var(--text); font-variant-numeric: tabular-nums; }
  .cactions { display: flex; gap: 10px; align-items: center; margin-top: 4px; }
  .statstrip { display: flex; flex-wrap: wrap; gap: 8px 18px; padding: 8px 16px;
    border-bottom: 1px solid var(--border); font-size: 13px; color: var(--muted); }
  .statstrip .s b { color: var(--text); }
  .grid { flex: 1; min-height: 0; display: flex; }
  .map { flex: 1; min-width: 0; height: 100%; }
  .col-resizer { width: 10px; flex: none; cursor: col-resize; background: var(--panel);
    display: flex; align-items: center; justify-content: center; touch-action: none; }
  .col-resizer::after { content: ''; width: 4px; height: 42px; border-radius: 3px;
    background: var(--border); transition: background 0.1s; }
  .col-resizer:hover::after, .col-resizer:active::after { background: var(--accent); }
  .side { flex: none; border-left: 1px solid var(--border); display: flex; flex-direction: column; min-height: 0; }
  .cams { display: flex; gap: 6px; padding: 8px; border-bottom: 1px solid var(--border); flex: none; }
  .cams .active { border-color: var(--accent); color: var(--accent); }
  .video-wrap { position: relative; flex: none; background: #000; }
  video { width: 100%; height: 100%; background: #000; object-fit: contain; display: block; }
  .row-resizer { height: 10px; flex: none; cursor: row-resize; background: var(--panel);
    display: flex; align-items: center; justify-content: center; touch-action: none; }
  .row-resizer::after { content: ''; height: 4px; width: 42px; border-radius: 3px;
    background: var(--border); transition: background 0.1s; }
  .row-resizer:hover::after, .row-resizer:active::after { background: var(--accent); }
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
  .arrow { font-size: 16px; color: #444; }
  .arrow.lit { color: #3fb950; }
  .ctrl { display: flex; align-items: center; justify-content: space-between; padding: 6px 12px; border-bottom: 1px solid var(--border); flex: none; }
  .rates { display: flex; gap: 4px; }
  .rate { padding: 3px 8px; font-size: 12px; }
  .rate.active { border-color: var(--accent); color: var(--accent); }
  .clock { padding: 6px 12px; border-bottom: 1px solid var(--border); }
  .events { flex: 1; min-height: 0; overflow: auto; padding: 8px; }
  .ev-head { font-weight: 600; margin: 4px 6px 8px; }
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
    .grid { flex-direction: column; }
    .map { height: 40%; flex: none; }
    .col-resizer { display: none; }
    .side { width: 100% !important; border-left: 0; border-top: 1px solid var(--border); flex: 1; }
  }
</style>
