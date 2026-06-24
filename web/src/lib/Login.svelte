<script>
  import { api, setSession } from './api.js';

  let { onLogin } = $props();
  let username = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e) {
    e.preventDefault();
    error = '';
    busy = true;
    try {
      const r = await api.login(username, password);
      setSession(r.access_token, {
        identity: r.identity,
        username: r.username,
        is_admin: r.is_admin,
      });
      onLogin(r.access_token, { username: r.username });
    } catch (err) {
      error = err.message || 'login failed';
    } finally {
      busy = false;
    }
  }
</script>

<div class="wrap">
  <form onsubmit={submit}>
    <h2>Sign in</h2>
    <label>Username<input bind:value={username} autocomplete="username" /></label>
    <label>Password<input type="password" bind:value={password} autocomplete="current-password" /></label>
    {#if error}<div class="error">{error}</div>{/if}
    <button disabled={busy || !username || !password}>{busy ? '…' : 'Log in'}</button>
  </form>
</div>

<style>
  .wrap { display: grid; place-items: center; height: 100%; }
  form {
    background: var(--panel); border: 1px solid var(--border); border-radius: 10px;
    padding: 24px; width: 320px; display: flex; flex-direction: column; gap: 12px;
  }
  h2 { margin: 0 0 6px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--muted); }
</style>
