<script>
  import { api, getToken, getUser, clearSession } from './lib/api.js';
  import Login from './lib/Login.svelte';
  import Drives from './lib/Drives.svelte';
  import Drive from './lib/Drive.svelte';
  import Settings from './lib/Settings.svelte';
  import DeviceSettings from './lib/DeviceSettings.svelte';
  import Stats from './lib/Stats.svelte';
  import Queues from './lib/Queues.svelte';
  import Account from './lib/Account.svelte';

  let token = $state(getToken());
  let user = $state(getUser());
  let selected = $state(null); // { route }
  let view = $state('drives'); // 'drives' | 'stats' | 'queues' | 'account' | 'device' | 'settings'
  let menuOpen = $state(false); // mobile nav dropdown

  // A `?pair=<token>` in the URL (e.g. scanned device QR) pairs once logged in.
  let pendingPair = new URLSearchParams(location.search).get('pair');
  let banner = $state('');

  // A `?share=<fullname>` link opens one drive read-only, no login required.
  let shareFullname = $state(new URLSearchParams(location.search).get('share'));
  let shareRoute = $state(null);
  let shareErr = $state('');
  $effect(() => {
    if (!shareFullname) return;
    api.routeInfo(shareFullname)
      .then((r) => { shareRoute = r; })
      .catch(() => { shareErr = 'This drive isn’t shared (the link may have been turned off).'; });
  });
  function exitShare() {
    shareRoute = null;
    shareErr = '';
    shareFullname = null;
    history.replaceState(null, '', location.pathname);
  }

  // Sync + encoding queue counters, polled while logged in.
  let queue = $state({ drives: 0, files: 0 });
  let enc = $state({ building: 0, current: null });
  $effect(() => {
    if (!token) return;
    let stop = false;
    const tick = async () => {
      try { queue = await api.syncQueue(); } catch {}
      try { enc = await api.movieQueue(); } catch {}
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
    menuOpen = false;
  }
  function goDrives() {
    selected = null;
    view = 'drives';
    menuOpen = false;
  }
  // Switch view and close the mobile menu.
  function navTo(v) {
    view = v;
    menuOpen = false;
  }
</script>

<div class="layout">
  <header>
    <div class="brand">home<span>connect</span></div>
    {#if token}
      <div class="right">
        <button class="syncing" class:active={queue.files > 0} class:sel={view === 'queues'} title="Drive sync status — click for the queue" onclick={() => navTo('queues')}>
          {#if queue.files > 0}
            <span class="spin">⟳</span>
            {queue.drives} drive{queue.drives === 1 ? '' : 's'} · {queue.files} file{queue.files === 1 ? '' : 's'}
          {:else}
            <span class="ok-dot"></span> Synced
          {/if}
        </button>
        {#if enc.building > 0}
          <button class="syncing active" class:sel={view === 'queues'} title={enc.current ? `Encoding ${enc.current} — click for the queue` : 'Encoding movies'} onclick={() => navTo('queues')}>
            <span class="spin">⟳</span>
            Encoding {enc.building} movie{enc.building === 1 ? '' : 's'}
          </button>
        {/if}
        <button class="menu-toggle ghost" aria-label="Menu" aria-expanded={menuOpen} onclick={() => (menuOpen = !menuOpen)}>☰</button>
        <nav class="nav" class:open={menuOpen}>
          <button class="ghost" class:active={view === 'drives'} onclick={goDrives}>Drives</button>
          <button class="ghost" class:active={view === 'stats'} onclick={() => navTo('stats')}>Stats</button>
          {#if user?.is_admin}
            <button class="ghost" class:active={view === 'device'} onclick={() => navTo('device')}>Device</button>
            <button class="ghost" class:active={view === 'settings'} onclick={() => navTo('settings')}>Settings</button>
          {/if}
          <button class="ghost" class:active={view === 'account'} title="Account & users" onclick={() => navTo('account')}>{user?.username ?? 'Account'}</button>
          <button class="ghost" onclick={logout}>Log out</button>
        </nav>
      </div>
      {#if menuOpen}<button class="backdrop" aria-label="Close menu" onclick={() => (menuOpen = false)}></button>{/if}
    {:else if shareFullname}
      <div class="right"><button class="ghost" onclick={exitShare}>Log in</button></div>
    {/if}
  </header>

  {#if banner}<div class="banner">{banner}</div>{/if}

  <main>
    {#if shareRoute}
      {#key shareRoute.fullname}
        <Drive route={shareRoute} readonly onback={exitShare} />
      {/key}
    {:else if shareErr}
      <div class="share-msg">
        <p class="muted">{shareErr}</p>
        <button class="ghost" onclick={exitShare}>Go to homeconnect</button>
      </div>
    {:else if !token}
      <Login {onLogin} />
    {:else if view === 'settings'}
      <Settings />
    {:else if view === 'device'}
      <DeviceSettings />
    {:else if view === 'stats'}
      <Stats />
    {:else if view === 'queues'}
      <Queues />
    {:else if view === 'account'}
      <Account />
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
    position: relative;
    display: flex; align-items: center; justify-content: space-between;
    padding: 10px 18px; border-bottom: 1px solid var(--border); background: var(--panel);
  }
  .brand { font-weight: 700; font-size: 18px; }
  .brand span { color: var(--accent); }
  .right { display: flex; align-items: center; gap: 12px; }
  .nav { display: flex; align-items: center; gap: 12px; }
  .right .active { border-color: var(--accent); color: var(--text); }
  /* The hamburger is desktop-hidden; the nav shows inline. */
  .menu-toggle { display: none; font-size: 18px; line-height: 1; padding: 6px 10px; }
  .backdrop { display: none; }
  .syncing { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--muted);
    border: 1px solid var(--border); border-radius: 999px; padding: 3px 10px;
    background: none; cursor: pointer; font-family: inherit; }
  .syncing:hover { border-color: var(--accent); }
  .syncing.active { color: var(--accent); border-color: var(--accent); }
  .syncing.sel { background: var(--panel-2); }
  .ok-dot { width: 7px; height: 7px; border-radius: 50%; background: #3fb950; }
  .spin { display: inline-block; animation: spin 1.4s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  main { flex: 1; min-height: 0; overflow: auto; }
  .banner { background: var(--accent); color: #fff; padding: 8px 16px; font-size: 13px; }
  .share-msg { padding: 40px 18px; text-align: center; display: flex; flex-direction: column; align-items: center; gap: 12px; }

  /* Mobile: collapse the nav buttons behind a hamburger; keep the sync badge. */
  @media (max-width: 640px) {
    header { padding: 10px 14px; }
    .right { gap: 8px; }
    .menu-toggle { display: inline-flex; }
    .nav {
      display: none;
      position: absolute; top: 100%; right: 8px; z-index: 6;
      flex-direction: column; align-items: stretch; gap: 6px;
      min-width: 180px; padding: 8px;
      background: var(--panel); border: 1px solid var(--border);
      border-radius: 10px; box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    }
    .nav.open { display: flex; }
    .nav button { width: 100%; text-align: left; padding: 10px 12px; }
    .backdrop { display: block; position: fixed; inset: 0; z-index: 5; background: transparent; border: 0; }
  }
</style>
