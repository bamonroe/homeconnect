<script>
  import { api } from './api.js';
  import { TYPE_LABELS } from './format.js';

  let cfg = $state(null);
  let error = $state('');
  let msg = $state('');
  let busy = $state(false);
  let tc = $state(null); // { current, devices: [{value,label,encodes}] }
  let sync = $state(null); // { enabled, interval_secs, types:[], all_types:[] }
  let encode = $state(null); // { enabled, interval_secs }
  let ignoreRules = $state([]); // [{ conditions: [{field, op, value}] }]
  let ignoreMsg = $state('');

  async function load() {
    error = '';
    try {
      // Fetch concurrently — one round trip instead of several sequential ones.
      const [c, t, s, e, ir] = await Promise.all([
        api.retention(), api.transcode(), api.syncSettings(), api.encodingSettings(), api.ignoreRules(),
      ]);
      cfg = c; tc = t; sync = s; encode = e; ignoreRules = ir.rules || [];
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

  const SCALE_LABELS = { native: 'Full (1344×760)', '854': '854×480', '640': '640×360' };
  async function saveEncodeQuality() {
    busy = true; error = ''; msg = '';
    try {
      const r = await api.setEncoding({
        scale: encode.scale, crf: Math.max(16, Math.min(35, Number(encode.crf) || 23)), preset: encode.preset,
      });
      encode = { ...encode, ...r };
      msg = 'Encode settings saved. Use “Re-encode all” to apply them to existing movies.';
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }
  function addRule() { ignoreRules = [...ignoreRules, { conditions: [{ field: 'minutes', op: 'lt', value: 1 }] }]; }
  function removeRule(ri) { ignoreRules = ignoreRules.filter((_, i) => i !== ri); }
  function addCond(ri) { ignoreRules[ri].conditions = [...ignoreRules[ri].conditions, { field: 'miles', op: 'lt', value: 0.2 }]; ignoreRules = [...ignoreRules]; }
  function removeCond(ri, ci) { ignoreRules[ri].conditions = ignoreRules[ri].conditions.filter((_, i) => i !== ci); ignoreRules = [...ignoreRules]; }
  async function saveIgnore() {
    busy = true; error = ''; msg = ''; ignoreMsg = '';
    try {
      // drop empty rules; coerce values to numbers
      const rules = ignoreRules
        .map((r) => ({ conditions: r.conditions.map((c) => ({ field: c.field, op: c.op, value: Number(c.value) || 0 })) }))
        .filter((r) => r.conditions.length);
      const res = await api.setIgnoreRules(rules);
      ignoreRules = res.rules || [];
      ignoreMsg = 'Ignore rules saved.';
    } catch (e) {
      ignoreMsg = e.message;
    } finally {
      busy = false;
    }
  }

  async function reencodeAll() {
    if (!confirm('Re-encode every movie with the current settings? Existing movies are rebuilt in the background (deleted ones stay deleted).')) return;
    busy = true; error = ''; msg = '';
    try {
      const r = await api.reencodeMovies();
      msg = `Re-encoding ${r.cleared} movie${r.cleared === 1 ? '' : 's'} — watch the Encoding badge.`;
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

        <h4>Encode quality</h4>
        <p class="muted small">
          How the full-res cameras are encoded (the Road movie is a fast stream copy, unaffected).
          Lower quality number = better &amp; bigger; full-res road footage is ~12 MB/min at 23.
        </p>
        <label>Resolution
          <select bind:value={encode.scale} disabled={busy}>
            {#each encode.scale_options ?? ['native'] as s}
              <option value={s}>{SCALE_LABELS[s] ?? s}</option>
            {/each}
          </select>
        </label>
        <label>Quality (CRF, 16–35; lower = better)
          <input type="number" min="16" max="35" step="1" bind:value={encode.crf} disabled={busy} />
        </label>
        <label>CPU preset
          <select bind:value={encode.preset} disabled={busy}>
            {#each encode.preset_options ?? ['veryfast'] as p}
              <option value={p}>{p}</option>
            {/each}
          </select>
        </label>
        <div class="actions">
          <button disabled={busy} onclick={saveEncodeQuality}>Save quality</button>
          <button class="ghost" disabled={busy} onclick={reencodeAll}>Re-encode all movies</button>
        </div>
      </div>
    {/if}

    <div class="card">
      <h3>Ignore rules</h3>
      <p class="muted small">
        Hide trivial drives from the Drives list and the Stats totals. A drive is ignored if it
        matches <b>any</b> rule, and a rule matches when <b>all</b> of its conditions are true.
        Nothing is deleted — clearing the rules brings every drive back.
      </p>
      {#each ignoreRules as rule, ri}
        {#if ri > 0}<div class="oror">OR</div>{/if}
        <div class="rule">
          {#each rule.conditions as c, ci}
            {#if ci > 0}<span class="andlbl">and</span>{/if}
            <span class="cond">
              <select bind:value={c.field} disabled={busy}>
                <option value="miles">miles</option>
                <option value="minutes">minutes</option>
              </select>
              <select bind:value={c.op} disabled={busy}>
                <option value="lt">&lt;</option>
                <option value="le">≤</option>
                <option value="gt">&gt;</option>
                <option value="ge">≥</option>
              </select>
              <input type="number" step="0.1" min="0" bind:value={c.value} disabled={busy} />
              <button class="x" title="Remove condition" onclick={() => removeCond(ri, ci)}>✕</button>
            </span>
          {/each}
          <button class="ghost xs" disabled={busy} onclick={() => addCond(ri)}>+ and</button>
          <button class="ghost xs" disabled={busy} onclick={() => removeRule(ri)}>remove rule</button>
        </div>
      {/each}
      <div class="actions">
        <button class="ghost" disabled={busy} onclick={addRule}>+ Add rule</button>
        <button disabled={busy} onclick={saveIgnore}>Save rules</button>
        {#if ignoreMsg}<span class="muted small">{ignoreMsg}</span>{/if}
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
  h4 { margin: 16px 0 6px; font-size: 13px; }
  .checks { display: flex; flex-direction: column; gap: 8px; margin-bottom: 12px; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; margin-bottom: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--muted); margin-bottom: 12px; }
  label.toggle { flex-direction: row; align-items: center; gap: 10px; margin-bottom: 0; font-size: 14px; color: var(--text); }
  label.toggle input { width: 18px; height: 18px; }
  .actions { display: flex; gap: 10px; margin-top: 6px; align-items: center; flex-wrap: wrap; }
  .rule { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 8px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px; }
  .oror { font-size: 11px; font-weight: 700; color: var(--muted); margin: 6px 0 6px 4px; }
  .andlbl { font-size: 12px; color: var(--muted); }
  .cond { display: inline-flex; align-items: center; gap: 4px; }
  .cond select, .cond input { width: auto; }
  .cond input[type="number"] { width: 72px; }
  .cond .x { background: none; border: none; color: var(--muted); cursor: pointer; padding: 0 2px; font-size: 12px; }
  .cond .x:hover { color: #f85149; }
  .xs { padding: 3px 9px; font-size: 12px; }
  .stat { font-size: 16px; }
  .small { font-size: 12px; }
  .ok { color: #3fb950; }
</style>
