<script>
  import { api } from './api.js';

  let { onback } = $props();

  let devices = $state([]);
  let dev = $state(null); // selected dongle
  let dp = $state(null); // { online, specs, values, pending }
  let loading = $state(true);
  let error = $state('');

  async function loadDevices() {
    loading = true; error = '';
    try {
      devices = await api.devices();
      if (devices.length && !dev) dev = devices[0].dongle_id;
      if (dev) await loadParams();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadParams() {
    msg = '';
    dp = await api.deviceParams(dev);
  }

  async function pickDevice(d) {
    dev = d; error = ''; msg = '';
    try { await loadParams(); } catch (e) { error = e.message; }
  }

  let syncTimer;
  // Edits update the cache instantly and queue a write; the device gets it now
  // (if online) or on its next connect. So the UI flips immediately.
  async function setParam(key, value) {
    const prev = dp.values[key];
    error = '';
    dp.values[key] = value;
    if (!dp.pending.includes(key)) dp.pending = [...dp.pending, key];
    dp = { ...dp };
    try {
      const r = await api.setDeviceParam(dev, key, value);
      // When online, re-pull shortly after to clear the "pending" dots once the
      // background flush to the device has landed.
      if (r.online) {
        clearTimeout(syncTimer);
        syncTimer = setTimeout(async () => {
          try { dp = await api.deviceParams(dev); } catch {}
        }, 2000);
      }
    } catch (e) {
      error = e.message;
      dp.values[key] = prev; // revert on rejection (e.g. invalid value)
      dp.pending = dp.pending.filter((k) => k !== key);
      dp = { ...dp };
    }
  }

  let pendingCount = $derived(dp ? dp.pending.length : 0);

  let groups = $derived(dp ? [...new Set(dp.specs.map((s) => s.group))] : []);

  // A conditional setting is active only when its controlling param has an
  // enabling value (matches how sunnypilot greys these out).
  let openHelp = $state({});
  function toggleHelp(e, key) {
    e.preventDefault();
    e.stopPropagation();
    openHelp[key] = !openHelp[key];
    openHelp = { ...openHelp };
  }

  function active(s) {
    if (!s.depends_on) return true;
    // Unset params read as '' — treat as '0' (off/default) for the condition.
    const cur = dp.values[s.depends_on.key] || '0';
    return s.depends_on.values.includes(cur);
  }

  $effect(() => { loadDevices(); });
</script>

<div class="page">
  <div class="bar">
    <button class="ghost" onclick={onback}>← Drives</button>
    <h2>Device settings</h2>
    {#if devices.length > 1}
      <div class="devices">
        {#each devices as d}
          <button class="ghost" class:active={d.dongle_id === dev} onclick={() => pickDevice(d.dongle_id)}>
            {d.alias || d.dongle_id}
            <span class="dot" class:on={d.online}></span>
          </button>
        {/each}
      </div>
    {/if}
  </div>

  {#if error}<p class="error">{error}</p>{/if}

  {#if loading}
    <p class="muted">Loading…</p>
  {:else if !devices.length}
    <p class="muted">No devices yet. Add and claim a device first.</p>
  {:else if dp}
    <p class="muted small">
      Changes save instantly and apply to the device when it's online{dp.online ? '' : ' — it’s offline now'}.
      Most settings take effect on the next ignition.
    </p>
    {#if pendingCount > 0}
      <p class="pending-banner small">
        {pendingCount} change{pendingCount === 1 ? '' : 's'} pending —
        {dp.online ? 'applying to the device…' : 'will apply when it reconnects.'}
      </p>
    {/if}

    {#each groups as g}
      <div class="card">
        <h3>{g}</h3>
        {#each dp.specs.filter((s) => s.group === g) as s}
          {@const on = active(s)}
          {@const lbl = `${s.label}`}
          <div class="item">
            {#if s.kind === 'info'}
              <div class="drow">
                <span class="lbl">{lbl}{#if s.help}<button class="help" type="button" onclick={(e) => toggleHelp(e, s.key)}>?</button>{/if}{#if dp.pending.includes(s.key)}<span class="pend" title="Pending — not yet sent to the device"></span>{/if}</span>
                <span class="muted">{dp.values[s.key] || '—'}</span>
              </div>
            {:else if s.kind === 'bool'}
              <label class="drow" class:dim={!on}>
                <span class="lbl">{lbl}{#if s.help}<button class="help" type="button" onclick={(e) => toggleHelp(e, s.key)}>?</button>{/if}{#if dp.pending.includes(s.key)}<span class="pend" title="Pending — not yet sent to the device"></span>{/if}</span>
                <input type="checkbox" checked={dp.values[s.key] === '1'} disabled={!on}
                  onchange={(e) => setParam(s.key, e.currentTarget.checked ? '1' : '0')} />
              </label>
            {:else if s.kind === 'enum'}
              <label class="drow" class:dim={!on}>
                <span class="lbl">{lbl}{#if s.help}<button class="help" type="button" onclick={(e) => toggleHelp(e, s.key)}>?</button>{/if}{#if dp.pending.includes(s.key)}<span class="pend" title="Pending — not yet sent to the device"></span>{/if}</span>
                <select value={dp.values[s.key] ?? ''} disabled={!on}
                  onchange={(e) => setParam(s.key, e.currentTarget.value)}>
                  {#each s.options as o}<option value={o.value}>{o.label}</option>{/each}
                </select>
              </label>
            {:else if s.kind === 'int'}
              <label class="drow" class:dim={!on}>
                <span class="lbl">{lbl}{#if s.help}<button class="help" type="button" onclick={(e) => toggleHelp(e, s.key)}>?</button>{/if}{#if dp.pending.includes(s.key)}<span class="pend" title="Pending — not yet sent to the device"></span>{/if}</span>
                <span class="num">
                  <input type="number" min={s.min} max={s.max} step={s.step || 1}
                    value={dp.values[s.key] ?? ''} disabled={!on}
                    onchange={(e) => setParam(s.key, e.currentTarget.value)} />
                  {#if s.unit}<span class="muted small">{s.unit}</span>{/if}
                </span>
              </label>
            {/if}
            {#if openHelp[s.key] && s.help}<div class="help-text muted small">{s.help}</div>{/if}
          </div>
        {/each}
      </div>
    {/each}
  {/if}
</div>

<style>
  .page { padding: 18px; max-width: 560px; margin: 0 auto; }
  .bar { display: flex; align-items: center; gap: 14px; margin-bottom: 16px; flex-wrap: wrap; }
  h2 { margin: 0; }
  .devices { display: flex; gap: 8px; flex-wrap: wrap; }
  .devices .active { border-color: var(--accent); }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #6e7681; margin-left: 6px; }
  .dot.on { background: #3fb950; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 4px 16px; margin-bottom: 14px; }
  h3 { margin: 12px 0 4px; font-size: 13px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.04em; }
  .small { font-size: 12px; }
  .ok { color: #3fb950; }
  .item { border-bottom: 1px solid var(--border); }
  .item:last-child { border-bottom: none; }
  .drow { display: flex; align-items: center; justify-content: space-between; gap: 14px; padding: 12px 0; font-size: 14px; }
  .drow input, .drow select { width: auto; flex: none; }
  .lbl { display: inline-flex; align-items: center; gap: 8px; }
  .dim { opacity: 0.4; }
  .pend { width: 7px; height: 7px; border-radius: 50%; background: #d29922; flex: none; }
  .pending-banner { color: #d29922; margin-top: -4px; }
  .help { width: 18px; height: 18px; border-radius: 50%; border: 1px solid var(--border); background: transparent;
    color: var(--muted); font-size: 11px; line-height: 1; cursor: pointer; flex: none; padding: 0; }
  .help:hover { color: var(--text); border-color: var(--accent); }
  .help-text { padding: 0 0 12px; max-width: 90%; line-height: 1.5; }
  .num { display: inline-flex; align-items: center; gap: 6px; }
  .num input { width: 80px; text-align: right; }
</style>
