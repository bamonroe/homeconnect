<script>
  import { api } from './api.js';

  let { onpaired, onclose } = $props();

  let unpaired = $state([]);
  let loading = $state(true);
  let error = $state('');
  let token = $state('');
  let busy = $state(false);
  let copied = $state(false);

  const onboardCmd = `ssh comma@<device-ip> 'curl -fsSL ${location.origin}/onboard.sh | bash -s -- --reboot'`;
  async function copyCmd() {
    try {
      await navigator.clipboard.writeText(onboardCmd);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {}
  }

  async function load() {
    loading = true;
    error = '';
    try {
      unpaired = await api.unpairedDevices();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function claim(dongle) {
    busy = true;
    error = '';
    try {
      await api.claim(dongle);
      onpaired();
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  async function pairWithToken(e) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      await api.pair(token);
      onpaired();
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    load();
  });
</script>

<div class="overlay" onclick={onclose}>
  <div class="modal" onclick={(e) => e.stopPropagation()}>
    <div class="head">
      <h2>Add a device</h2>
      <button class="ghost" onclick={onclose}>✕</button>
    </div>

    {#if error}<div class="error">{error}</div>{/if}

    <section>
      <h3>Point a new device here</h3>
      <p class="muted small">
        Run this on the comma (or over SSH). It repoints the device at this server and lets it
        register; then claim it below.
      </p>
      <div class="cmd">
        <code>{onboardCmd}</code>
        <button class="ghost" onclick={copyCmd}>{copied ? 'Copied' : 'Copy'}</button>
      </div>
      <p class="muted small">The device reboots itself, then registers here in a minute or two — refresh this list and Claim it.</p>
    </section>

    <section>
      <h3>Devices waiting to be claimed</h3>
      {#if loading}
        <p class="muted">Looking…</p>
      {:else if !unpaired.length}
        <p class="muted small">
          None. A device shows up here once it has connected to this server (registered) but
          isn’t paired to anyone yet. If your device doesn’t appear, point it at this server and
          let it register, or use a pairing code below.
        </p>
      {:else}
        <div class="list">
          {#each unpaired as d}
            <div class="row">
              <div>
                <div class="mono">{d.dongle_id}</div>
                <div class="muted small">{d.device_type || 'device'} · {d.online ? 'online' : 'offline'}</div>
              </div>
              <button disabled={busy} onclick={() => claim(d.dongle_id)}>Claim</button>
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <h3>Or pair with a code</h3>
      <p class="muted small">Paste the pairing code (or the full pairing URL) shown on the device.</p>
      <form onsubmit={pairWithToken}>
        <input bind:value={token} placeholder="pairing token or ...?pair=… URL" />
        <button disabled={busy || !token}>Pair</button>
      </form>
    </section>
  </div>
</div>

<style>
  .overlay {
    position: fixed; inset: 0; background: rgba(0,0,0,0.55);
    display: grid; place-items: center; z-index: 50;
  }
  .modal {
    background: var(--panel); border: 1px solid var(--border); border-radius: 12px;
    width: 460px; max-width: calc(100vw - 32px); padding: 18px;
    max-height: calc(100vh - 64px); overflow: auto;
  }
  .head { display: flex; align-items: center; justify-content: space-between; }
  h2 { margin: 0; }
  h3 { margin: 18px 0 8px; font-size: 14px; }
  section + section { border-top: 1px solid var(--border); }
  .list { display: grid; gap: 8px; }
  .row {
    display: flex; align-items: center; justify-content: space-between;
    background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px; padding: 10px;
  }
  form { display: flex; gap: 8px; }
  .mono { font-family: ui-monospace, monospace; }
  .small { font-size: 12px; }
  .cmd { display: flex; gap: 8px; align-items: center; background: var(--panel-2); border: 1px solid var(--border); border-radius: 8px; padding: 8px 10px; }
  .cmd code { font-family: ui-monospace, monospace; font-size: 12px; overflow-x: auto; white-space: nowrap; flex: 1; }
</style>
