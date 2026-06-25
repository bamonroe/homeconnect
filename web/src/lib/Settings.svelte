<script>
  import { api } from './api.js';

  let cfg = $state(null);
  let error = $state('');
  let msg = $state('');
  let busy = $state(false);
  let tc = $state(null); // { current, devices: [{value,label,encodes}] }
  let sync = $state(null); // { enabled, interval_secs, types:[], all_types:[] }
  let encode = $state(null); // { enabled, interval_secs }

  const TYPE_LABELS = {
    qcamera: 'Road (qcamera)',
    fcamera: 'Road HD (fcamera)',
    dcamera: 'Driver (dcamera)',
    ecamera: 'Wide (ecamera)',
    rlog: 'Raw log (rlog)',
  };

  async function load() {
    error = '';
    try {
      // Fetch concurrently — one round trip instead of several sequential ones.
      const [c, t, s, e] = await Promise.all([
        api.retention(), api.transcode(), api.syncSettings(), api.encodingSettings(),
      ]);
      cfg = c; tc = t; sync = s; encode = e;
    } catch (e) {
      error = e.message;
    }
  }

  async function toggleSync(e) {
    const on = e.currentTarget.checked;
    busy = true; error = ''; msg = '';
    try {
      await api.setSync({ enabled: on });
      sync.enabled = on;
      msg = on ? 'Automatic sync turned on.' : 'Automatic sync turned off.';
    } catch (err) {
      error = err.message;
      e.currentTarget.checked = !on; // revert on failure
    } finally {
      busy = false;
    }
  }

  async function toggleAutoprune(e) {
    const on = e.currentTarget.checked;
    busy = true; error = ''; msg = '';
    try {
      await api.setSync({ autoprune: on });
      sync.autoprune = on;
      msg = on ? 'Device copies will be deleted after sync.' : 'Device copies will be kept.';
    } catch (err) {
      error = err.message;
      e.currentTarget.checked = !on; // revert on failure
    } finally {
      busy = false;
    }
  }

  function onType(t, e) {
    sync.types = e.currentTarget.checked
      ? [...sync.types, t]
      : sync.types.filter((x) => x !== t);
  }

  async function saveTypes() {
    busy = true; error = ''; msg = '';
    try {
      const r = await api.setSync({ types: sync.types });
      sync.types = r.types;
      msg = 'Default sync data saved.';
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function saveInterval() {
    busy = true; error = ''; msg = '';
    try {
      const r = await api.setSync({ interval_secs: Math.max(0, Number(sync.interval_secs) || 0) });
      sync.interval_secs = r.interval_secs;
      msg = r.interval_secs === 0
        ? 'Periodic check off — sync now only runs when the device connects.'
        : `Periodic check set to every ${r.interval_secs}s.`;
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function toggleEncoding(e) {
    const on = e.currentTarget.checked;
    busy = true; error = ''; msg = '';
    try {
      await api.setEncoding({ enabled: on });
      encode.enabled = on;
      msg = on ? 'Movie encoding turned on.' : 'Movie encoding turned off.';
    } catch (err) {
      error = err.message;
      e.currentTarget.checked = !on; // revert on failure
    } finally {
      busy = false;
    }
  }

  async function saveEncodeInterval() {
    busy = true; error = ''; msg = '';
    try {
      const r = await api.setEncoding({ interval_secs: Math.max(0, Number(encode.interval_secs) || 0) });
      encode.interval_secs = r.interval_secs;
      msg = `Encoder checks for new movies every ${r.interval_secs}s.`;
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function saveTranscode() {
    busy = true; error = ''; msg = '';
    try {
      await api.setTranscode(tc.current);
      msg = 'Transcode device saved.';
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  async function save() {
    busy = true; error = ''; msg = '';
    try {
      await api.setRetention({
        days: Number(cfg.days),
        max_drives: Number(cfg.max_drives),
        max_gb: Number(cfg.max_gb),
      });
      msg = 'Saved.';
      await load();
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  async function runNow() {
    busy = true; error = ''; msg = '';
    try {
      const r = await api.runRetention();
      msg = `Cleanup ran — ${r.deleted} drive(s) removed.`;
      await load();
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  $effect(() => { load(); });
</script>

<div class="page">
  <div class="bar">
    <h2>Settings</h2>
  </div>

  {#if error}<p class="error">{error}</p>{/if}
  {#if msg}<p class="ok">{msg}</p>{/if}

  {#if !cfg}
    <p class="muted">Loading…</p>
  {:else}
    <div class="card">
      <h3>Storage</h3>
      <div class="stat">
        <span>{cfg.storage_gb.toFixed(2)} GB used</span>
        <span class="muted">· {cfg.route_count} drives stored</span>
      </div>
    </div>

    {#if sync}
      <div class="card">
        <h3>Automatic drive sync</h3>
        <p class="muted small">
          Pull new drives off the device over SSH automatically (when it connects, and every
          minute while it's online). Turn off to stop automatic pulls — you can still use
          <strong>Sync now</strong> and <strong>Pull full-res</strong> on demand.
        </p>
        <label class="toggle">
          <input type="checkbox" checked={sync.enabled} disabled={busy} onchange={toggleSync} />
          <span>{sync.enabled ? 'On' : 'Off'}</span>
        </label>
        <label>Re-check online device every (seconds) — 0 = only when it connects
          <input type="number" min="0" step="10" bind:value={sync.interval_secs} disabled={busy} />
        </label>
        <div class="actions">
          <button disabled={busy} onclick={saveInterval}>Save interval</button>
        </div>

        <h4>Data synced by default</h4>
        <p class="muted small">
          Which files automatic sync pulls. The driving log (telemetry, map, events) is always
          synced; the cameras and raw log are large — leave the full-res ones off to save space
          and pull them per drive on demand.
        </p>
        <div class="checks">
          {#each sync.all_types as t}
            <label class="toggle">
              <input type="checkbox" checked={sync.types.includes(t)} disabled={busy}
                onchange={(e) => onType(t, e)} />
              <span>{TYPE_LABELS[t] ?? t}</span>
            </label>
          {/each}
        </div>
        <div class="actions">
          <button disabled={busy} onclick={saveTypes}>Save default data</button>
        </div>

        <h4>Reclaim device storage</h4>
        <p class="muted small">
          After a file is safely pulled and stored here, delete the device's copy to free its
          storage. Only ever deletes files this server already holds — never anything unsynced.
        </p>
        <label class="toggle">
          <input type="checkbox" checked={sync.autoprune} disabled={busy} onchange={toggleAutoprune} />
          <span>{sync.autoprune ? 'Auto-delete device copies after sync' : 'Keep device copies (device rotates its own storage)'}</span>
        </label>
      </div>
    {/if}

    {#if encode}
      <div class="card">
        <h3>Movie encoding</h3>
        <p class="muted small">
          Stitch each fully-synced drive's segments into one watchable MP4 (with audio) in the
          background. Turn off to pause all encoding; the builder checks for new drives on the
          interval below. Runs independently of drive sync.
        </p>
        <label class="toggle">
          <input type="checkbox" checked={encode.enabled} disabled={busy} onchange={toggleEncoding} />
          <span>{encode.enabled ? 'On' : 'Off'}</span>
        </label>
        <label>Check for new drives to encode every (seconds)
          <input type="number" min="30" step="10" bind:value={encode.interval_secs} disabled={busy} />
        </label>
        <div class="actions">
          <button disabled={busy} onclick={saveEncodeInterval}>Save interval</button>
        </div>
      </div>
    {/if}

    {#if tc}
      <div class="card">
        <h3>Transcoding device</h3>
        <p class="muted small">
          Which device decodes/encodes the full-res &amp; driver cameras. A capable GPU is faster
          and frees the CPU when the server is busy; CPU always works as a fallback.
        </p>
        <label>Device
          <select bind:value={tc.current}>
            {#each tc.devices as d}
              <option value={d.value} disabled={!d.encodes}>
                {d.label}{d.encodes ? '' : ' — no H.264 encode'}
              </option>
            {/each}
          </select>
        </label>
        <div class="actions">
          <button disabled={busy} onclick={saveTranscode}>Save device</button>
        </div>
      </div>
    {/if}

    <div class="card">
      <h3>Retention policy</h3>
      <p class="muted small">Drives are pruned when any limit is exceeded. 0 = unlimited.</p>
      <label>Keep drives for (days)<input type="number" min="0" bind:value={cfg.days} /></label>
      <label>Max drives per device<input type="number" min="0" bind:value={cfg.max_drives} /></label>
      <label>Max total storage (GB)<input type="number" min="0" step="0.1" bind:value={cfg.max_gb} /></label>
      <div class="actions">
        <button disabled={busy} onclick={save}>Save</button>
        <button class="ghost" disabled={busy} onclick={runNow}>Run cleanup now</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .page { padding: 18px; max-width: 560px; margin: 0 auto; }
  .bar { display: flex; align-items: center; gap: 14px; margin-bottom: 16px; }
  h2 { margin: 0; }
  h3 { margin: 0 0 10px; font-size: 14px; }
  h4 { margin: 16px 0 6px; font-size: 13px; }
  .checks { display: flex; flex-direction: column; gap: 8px; margin-bottom: 12px; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; margin-bottom: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--muted); margin-bottom: 12px; }
  label.toggle { flex-direction: row; align-items: center; gap: 10px; margin-bottom: 0; font-size: 14px; color: var(--text); }
  label.toggle input { width: 18px; height: 18px; }
  .actions { display: flex; gap: 10px; margin-top: 6px; }
  .stat { font-size: 16px; }
  .small { font-size: 12px; }
  .ok { color: #3fb950; }
</style>
