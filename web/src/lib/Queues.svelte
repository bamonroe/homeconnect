<script>
  // Live view of background work: movies waiting to encode + drives/files waiting
  // to sync. Polls the same counters the header badges use, plus their item lists.
  import { api } from './api.js';

  let enc = $state({ building: 0, current: null, queued: [] });
  let sync = $state({ drives: 0, files: 0, items: [] });

  $effect(() => {
    let stop = false;
    const tick = async () => {
      try { enc = await api.movieQueue(); } catch {}
      try { sync = await api.syncQueue(); } catch {}
    };
    tick();
    const id = setInterval(() => { if (!stop) tick(); }, 2000);
    return () => { stop = true; clearInterval(id); };
  });

  const FILE_LABELS = {
    'qcamera.ts': 'Road', 'fcamera.hevc': 'Road HD', 'dcamera.hevc': 'Driver',
    'ecamera.hevc': 'Wide', 'qlog.zst': 'Log', 'qlog.bz2': 'Log',
    'rlog.zst': 'Raw log', 'rlog.bz2': 'Raw log',
  };
  const fileLabel = (f) => FILE_LABELS[f] ?? f;
</script>

<div class="queues">
  <h1>Queues</h1>

  <section class="card">
    <h2>Encoding {#if enc.building > 0}<span class="count">{enc.building}</span>{/if}</h2>
    <p class="muted small">Drives are stitched into watchable movies in the background.</p>
    {#if enc.current || (enc.queued && enc.queued.length)}
      <ul>
        {#if enc.current}
          <li class="cur"><span class="spin">⟳</span> {enc.current} <span class="muted">· encoding now</span></li>
        {/if}
        {#each enc.queued ?? [] as q}
          <li><span class="dot"></span> {q} <span class="muted">· queued</span></li>
        {/each}
      </ul>
    {:else}
      <p class="idle">Nothing encoding right now.</p>
    {/if}
  </section>

  <section class="card">
    <h2>Syncing {#if sync.files > 0}<span class="count">{sync.files}</span>{/if}</h2>
    <p class="muted small">Files pulled off the device over SSH (newest drives first to land).</p>
    {#if sync.items && sync.items.length}
      <ul>
        {#each sync.items as it}
          <li class:cur={it.in_flight}>
            {#if it.in_flight}<span class="spin">⟳</span>{:else}<span class="dot"></span>{/if}
            {it.ts} · {fileLabel(it.file)}
            <span class="muted">· {it.in_flight ? 'pulling now' : 'queued'}</span>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="idle">Nothing syncing right now.</p>
    {/if}
  </section>
</div>

<style>
  .queues { max-width: 720px; margin: 0 auto; padding: 20px 16px; }
  h1 { margin: 0 0 16px; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; padding: 16px 18px; margin-bottom: 16px; }
  h2 { margin: 0 0 4px; font-size: 16px; display: flex; align-items: center; gap: 8px; }
  .count { font-size: 12px; color: var(--accent); border: 1px solid var(--accent); border-radius: 999px; padding: 1px 9px; }
  .small { font-size: 12px; }
  ul { list-style: none; margin: 10px 0 0; padding: 0; display: grid; gap: 6px; }
  li { display: flex; align-items: center; gap: 8px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px; padding: 8px 11px; font-size: 14px; }
  li.cur { border-color: var(--accent); }
  .idle { color: var(--muted); margin: 8px 0 0; }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--muted); flex: none; }
  .spin { display: inline-block; animation: spin 1.4s linear infinite; color: var(--accent); }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
