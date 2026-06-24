<script>
  import { api } from './api.js';

  let { onback } = $props();

  let cfg = $state(null);
  let error = $state('');
  let msg = $state('');
  let busy = $state(false);
  let tc = $state(null); // { current, devices: [{value,label,encodes}] }

  async function load() {
    error = '';
    try {
      cfg = await api.retention();
      tc = await api.transcode();
    } catch (e) {
      error = e.message;
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
    <button class="ghost" onclick={onback}>← Drives</button>
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
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; margin-bottom: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--muted); margin-bottom: 12px; }
  .actions { display: flex; gap: 10px; margin-top: 6px; }
  .stat { font-size: 16px; }
  .small { font-size: 12px; }
  .ok { color: #3fb950; }
</style>
