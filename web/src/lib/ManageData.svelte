<script>
  import { api } from './api.js';

  let { route, onclose, onchanged } = $props();

  const types = [
    { id: 'qcamera', label: 'Road camera (qcamera.ts)', avail: (route.maxqcamera ?? -1) >= 0 },
    { id: 'fcamera', label: 'Road HD (fcamera.hevc)', avail: (route.maxcamera ?? -1) >= 0 },
    { id: 'dcamera', label: 'Driver camera (dcamera.hevc)', avail: (route.maxdcamera ?? -1) >= 0 },
    { id: 'ecamera', label: 'Wide camera (ecamera.hevc)', avail: (route.maxecamera ?? -1) >= 0 },
    { id: 'qlog', label: 'Logs (qlog)', avail: (route.maxqlog ?? -1) >= 0 },
    { id: 'rlog', label: 'Raw logs (rlog)', avail: (route.maxlog ?? -1) >= 0 },
  ].filter((t) => t.avail);

  let checked = $state({});
  let busy = $state(false);
  let error = $state('');

  let selected = $derived(types.filter((t) => checked[t.id]).map((t) => t.id));

  function download() {
    if (!selected.length) return;
    const a = document.createElement('a');
    a.href = api.downloadUrl(route.fullname, selected);
    a.download = '';
    document.body.appendChild(a);
    a.click();
    a.remove();
  }

  async function del() {
    if (!selected.length) return;
    if (!confirm(`Delete these off the server for this drive?\n\n${selected.join(', ')}\n\nThis cannot be undone.`)) return;
    busy = true;
    error = '';
    try {
      const r = await api.deleteData(route.fullname, selected);
      onchanged?.(r);
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay" onclick={onclose}>
  <div class="modal" onclick={(e) => e.stopPropagation()}>
    <div class="head">
      <h2>Manage data</h2>
      <button class="ghost" onclick={onclose}>✕</button>
    </div>
    <p class="muted small">Select the data to download or delete for this drive.</p>
    {#if error}<div class="error">{error}</div>{/if}

    <div class="list">
      {#each types as t}
        <label class="row">
          <input type="checkbox" bind:checked={checked[t.id]} />
          <span>{t.label}</span>
        </label>
      {/each}
    </div>

    <div class="actions">
      <button disabled={!selected.length} onclick={download}>Download (.zip)</button>
      <button class="danger" disabled={busy || !selected.length} onclick={del}>Delete from server</button>
    </div>
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.55); display: grid; place-items: center; z-index: 50; }
  .modal { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; width: 440px; max-width: calc(100vw - 32px); padding: 18px; }
  .head { display: flex; align-items: center; justify-content: space-between; }
  h2 { margin: 0; }
  .small { font-size: 12px; }
  .list { display: grid; gap: 6px; margin: 12px 0; }
  .row { display: flex; align-items: center; gap: 10px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px; padding: 9px 11px; cursor: pointer; }
  .row input { width: auto; }
  .actions { display: flex; gap: 10px; justify-content: flex-end; }
  .danger { background: #f85149; }
</style>
