<script>
  import { api } from './api.js';

  let { route, onclose, onchanged } = $props();

  const [dongle, ts] = route.fullname.split('|');

  // All data types, with whether each is already on the server. Cameras + rlog
  // can be pulled from the device; qlog is always synced (the route needs it).
  const types = [
    { id: 'qcamera', label: 'Road (qcamera)', pullable: true, synced: (route.maxqcamera ?? -1) >= 0 },
    { id: 'fcamera', label: 'Road HD (fcamera)', pullable: true, synced: (route.maxcamera ?? -1) >= 0 },
    { id: 'dcamera', label: 'Driver (dcamera)', pullable: true, synced: (route.maxdcamera ?? -1) >= 0 },
    { id: 'ecamera', label: 'Wide (ecamera)', pullable: true, synced: (route.maxecamera ?? -1) >= 0 },
    { id: 'rlog', label: 'Raw log (rlog)', pullable: true, synced: (route.maxlog ?? -1) >= 0 },
    { id: 'qlog', label: 'Driving log (qlog)', pullable: false, synced: (route.maxqlog ?? -1) >= 0 },
  ];

  let checked = $state({});
  let busy = $state(false);
  let error = $state('');
  let msg = $state('');

  let selected = $derived(types.filter((t) => checked[t.id]).map((t) => t.id));
  let selPullable = $derived(types.filter((t) => checked[t.id] && t.pullable).map((t) => t.id));
  let selSynced = $derived(types.filter((t) => checked[t.id] && t.synced).map((t) => t.id));

  async function pull() {
    if (!selPullable.length) return;
    busy = true; error = ''; msg = '';
    try {
      const s = await api.sync(dongle, { route: ts, types: selPullable });
      msg = s.online === false
        ? 'Device is offline — it’ll sync when it reconnects.'
        : 'Pull queued — progress shows up top. Reopen the drive once it lands.';
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  function download() {
    if (!selSynced.length) return;
    const a = document.createElement('a');
    a.href = api.downloadUrl(route.fullname, selSynced);
    a.download = '';
    document.body.appendChild(a);
    a.click();
    a.remove();
  }

  async function del() {
    if (!selSynced.length) return;
    if (!confirm(`Delete these off the server for this drive?\n\n${selSynced.join(', ')}\n\nThis cannot be undone.`)) return;
    busy = true; error = '';
    try {
      const r = await api.deleteData(route.fullname, selSynced);
      onchanged?.(r);
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay" onclick={onclose}>
  <div class="modal" role="dialog" tabindex="-1" onclick={(e) => e.stopPropagation()}>
    <div class="head">
      <h2>Manage data</h2>
      <button class="ghost" onclick={onclose}>✕</button>
    </div>
    <p class="muted small">
      Pick the data for this drive, then pull it from the device, download it, or delete it from
      the server.
    </p>
    {#if error}<div class="error">{error}</div>{/if}
    {#if msg}<div class="ok small">{msg}</div>{/if}

    <div class="list">
      {#each types as t}
        <label class="row">
          <input type="checkbox" bind:checked={checked[t.id]} />
          <span class="lbl">{t.label}</span>
          <span class="badge" class:on={t.synced}>{t.synced ? 'on server' : 'on device'}</span>
        </label>
      {/each}
    </div>

    <div class="actions">
      <button disabled={busy || !selPullable.length} onclick={pull}>
        {busy ? 'Working…' : 'Pull from device'}
      </button>
      <button class="ghost" disabled={!selSynced.length} onclick={download}>Download (.zip)</button>
      <button class="danger" disabled={busy || !selSynced.length} onclick={del}>Delete</button>
    </div>
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.55); display: grid; place-items: center; z-index: 50; }
  .modal { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; width: 460px; max-width: calc(100vw - 32px); padding: 18px; }
  .head { display: flex; align-items: center; justify-content: space-between; }
  h2 { margin: 0; }
  .small { font-size: 12px; }
  .ok { color: #3fb950; }
  .list { display: grid; gap: 6px; margin: 12px 0; }
  .row { display: flex; align-items: center; gap: 10px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px; padding: 9px 11px; cursor: pointer; }
  .row input { width: auto; }
  .lbl { flex: 1; }
  .badge { font-size: 11px; color: var(--muted); border: 1px solid var(--border); border-radius: 999px; padding: 1px 8px; }
  .badge.on { color: #3fb950; border-color: #2ea043; }
  .actions { display: flex; gap: 10px; justify-content: flex-end; flex-wrap: wrap; }
  .danger { background: #f85149; }
</style>
