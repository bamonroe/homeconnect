<script>
  import { api, getToken, getUser, clearSession } from './lib/api.js';
  import Login from './lib/Login.svelte';
  import Drives from './lib/Drives.svelte';
  import Drive from './lib/Drive.svelte';
  import Settings from './lib/Settings.svelte';
  import DeviceSettings from './lib/DeviceSettings.svelte';

  let token = $state(getToken());
  let user = $state(getUser());
  let selected = $state(null); // { route }
  let view = $state('drives'); // 'drives' | 'settings' | 'device'

  // A `?pair=<token>` in the URL (e.g. scanned device QR) pairs once logged in.
  let pendingPair = new URLSearchParams(location.search).get('pair');
  let banner = $state('');

  // Sync queue counter, polled while logged in.
  let queue = $state({ drives: 0, files: 0 });
  $effect(() => {
    if (!token) return;
    let stop = false;
    const tick = async () => {
      try { queue = await api.syncQueue(); } catch {}
    };
    tick();
    // Poll faster while there's work, slower when idle.
    const id = setInterval(() => { if (!stop) tick(); }, 3000);
    return () => { stop = true; clearInterval(id); };
  });

  $effect(() => {
    if (token && pendingPair) {
      const tok = pendingPair;
      pendingPair = null;
      api
        .pair(tok)
        .then((r) => { banner = `Device paired (${r.dongle_id}).`; })
        .catch((e) => { banner = `Pairing failed: ${e.message}`; })
        .finally(() => {
          // strip the pair param from the URL
          history.replaceState(null, '', location.pathname);
          setTimeout(() => (banner = ''), 6000);
        });
    }
  });

  function onLogin(tok) {
    token = tok;
    user = getUser();
  }
  function logout() {
    clearSession();
    token = null;
    user = null;
    selected = null;
    view = 'drives';
  }
</script>

<div class="layout">
  <header>
    <div class="brand">home<span>connect</span></div>
    {#if token}
      <div class="right">
        {#if queue.files > 0}
          <span class="syncing" title="Drives and files queued for syncing">
            <span class="spin">⟳</span>
            {queue.drives} drive{queue.drives === 1 ? '' : 's'} · {queue.files} file{queue.files === 1 ? '' : 's'}
          </span>
        {/if}
        {#if user?.is_admin}
          <button class="ghost" class:active={view === 'device'}
            onclick={() => (view = view === 'device' ? 'drives' : 'device')}>Device</button>
          <button class="ghost" class:active={view === 'settings'}
            onclick={() => (view = view === 'settings' ? 'drives' : 'settings')}>Settings</button>
        {/if}
        <span class="muted">{user?.username ?? ''}</span>
        <button class="ghost" onclick={logout}>Log out</button>
      </div>
    {/if}
  </header>

  {#if banner}<div class="banner">{banner}</div>{/if}

  <main>
    {#if !token}
      <Login {onLogin} />
    {:else if view === 'settings'}
      <Settings onback={() => (view = 'drives')} />
    {:else if view === 'device'}
      <DeviceSettings onback={() => (view = 'drives')} />
    {:else if selected}
      {#key selected.route.fullname}
        <Drive route={selected.route} onback={() => (selected = null)} />
      {/key}
    {:else}
      <Drives onopen={(route) => (selected = { route })} />
    {/if}
  </main>
</div>

<style>
  .layout { display: flex; flex-direction: column; height: 100%; }
  header {
    display: flex; align-items: center; justify-content: space-between;
    padding: 10px 18px; border-bottom: 1px solid var(--border); background: var(--panel);
  }
  .brand { font-weight: 700; font-size: 18px; }
  .brand span { color: var(--accent); }
  .right { display: flex; align-items: center; gap: 12px; }
  .right .active { border-color: var(--accent); color: var(--text); }
  .syncing { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--accent);
    border: 1px solid var(--border); border-radius: 999px; padding: 3px 10px; }
  .spin { display: inline-block; animation: spin 1.4s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  main { flex: 1; min-height: 0; overflow: auto; }
  .banner { background: var(--accent); color: #fff; padding: 8px 16px; font-size: 13px; }
</style>
