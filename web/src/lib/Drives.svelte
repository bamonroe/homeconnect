<script>
  import { api, getToken } from './api.js';
  import AddDevice from './AddDevice.svelte';

  let { onopen } = $props();

  let devices = $state([]);
  let dongle = $state(null);
  let routes = $state([]);
  let error = $state('');
  let loading = $state(true);
  let showAdd = $state(false);
  let syncing = $state(false);
  let syncMsg = $state('');

  async function syncNow() {
    if (!dongle || syncing) return;
    syncing = true;
    syncMsg = '';
    try {
      const s = await api.sync(dongle);
      syncMsg = s.online === false
        ? 'Device is offline — it’ll sync when it reconnects.'
        : 'Sync started — progress shows up top. New drives appear as they arrive.';
    } catch (e) {
      syncMsg = `Sync failed: ${e.message}`;
    } finally {
      syncing = false;
    }
  }

  async function onPaired() {
    showAdd = false;
    await loadDevices();
  }

  async function loadDevices() {
    loading = true;
    error = '';
    try {
      devices = await api.devices();
      if (devices.length && !dongle) dongle = devices[0].dongle_id;
      if (dongle) await loadRoutes();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadRoutes() {
    routes = await api.routes(dongle, { limit: 100 });
  }

  async function pickDevice(d) {
    dongle = d;
    error = '';
    try {
      await loadRoutes();
    } catch (e) {
      error = e.message;
    }
  }

  function fmtDate(ms) {
    if (!ms) return '—';
    return new Date(ms).toLocaleString();
  }
  function fmtLen(mi) {
    return mi ? `${mi.toFixed(1)} mi` : '—';
  }
  function fmtDur(r) {
    const s = r.start_time_utc_millis, e = r.end_time_utc_millis;
    if (!s || !e || e < s) return '';
    const min = Math.round((e - s) / 60000);
    return `${min} min`;
  }
  function spriteUrl(r) {
    const [dg, ts] = r.fullname.split('|');
    return `/connectdata/${dg}/${ts}/0/sprite.jpg?sig=${getToken()}`;
  }

  $effect(() => {
    loadDevices();
  });
</script>

<div class="page">
  <div class="toolbar">
    <div class="devices">
      {#each devices as d}
        <button class:active={d.dongle_id === dongle} class="ghost"
          onclick={() => pickDevice(d.dongle_id)}>
          {d.alias || d.dongle_id}
          <span class="dot" class:on={d.online}></span>
        </button>
      {/each}
    </div>
    <div class="actions">
      {#if dongle}
        <button class="ghost" onclick={syncNow} disabled={syncing}>
          {syncing ? 'Syncing…' : 'Sync now'}
        </button>
      {/if}
      <button onclick={() => (showAdd = true)}>+ Add device</button>
    </div>
  </div>

  {#if syncMsg}
    <p class="muted sync-msg">{syncMsg}</p>
  {/if}

  {#if showAdd}
    <AddDevice onpaired={onPaired} onclose={() => (showAdd = false)} />
  {/if}

  {#if loading}
    <p class="muted">Loading…</p>
  {:else if error}
    <p class="error">{error}</p>
  {:else if !routes.length}
    <p class="muted">No drives yet. Once your device uploads and the logs parse, they’ll appear here.</p>
  {:else}
    <div class="list">
      {#each routes as r}
        <button class="card" onclick={() => onopen(r)}>
          <div class="thumb">
            <img src={spriteUrl(r)} alt="" loading="lazy"
              onerror={(e) => (e.currentTarget.style.visibility = 'hidden')} />
          </div>
          <div class="meta">
            <div class="title">{fmtDate(r.start_time_utc_millis)}</div>
            <div class="sub muted">
              {fmtLen(r.length)} · {fmtDur(r)} · {r.platform || 'unknown'}
            </div>
          </div>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .page { padding: 18px; max-width: 920px; margin: 0 auto; }
  .toolbar { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 16px; }
  .actions { display: flex; gap: 8px; flex: none; }
  .sync-msg { margin: -6px 0 14px; }
  .devices { display: flex; gap: 8px; flex-wrap: wrap; }
  .devices .active { border-color: var(--accent); }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #6e7681; margin-left: 6px; }
  .dot.on { background: #3fb950; }
  .list { display: grid; gap: 10px; }
  .card {
    display: flex; gap: 14px; align-items: center; text-align: left;
    background: var(--panel); border: 1px solid var(--border); border-radius: 10px;
    padding: 10px; color: var(--text); cursor: pointer;
  }
  .card:hover { border-color: var(--accent); }
  .thumb {
    width: 128px; height: 80px; border-radius: 6px; overflow: hidden;
    background: var(--panel-2); flex: none; display: grid; place-items: center;
  }
  .thumb img { width: 100%; height: 100%; object-fit: cover; }
  .title { font-weight: 600; }
  .sub { font-size: 13px; margin-top: 3px; }
</style>
