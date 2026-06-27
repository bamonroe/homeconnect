<script>
  // Account view: every user can change their own password here; admins also get
  // a "Manage users" panel (add/remove users, toggle the admin flag, reset a
  // user's password). The "admin" flag is the whole permission model — admins see
  // every device + all settings, non-admins see only drives/stats.
  import { api, getUser } from './api.js';

  const me = getUser() ?? {};

  // --- self password change ---
  let cur = $state('');
  let nw = $state('');
  let confirm2 = $state('');
  let pwMsg = $state('');
  let pwErr = $state('');
  let pwBusy = $state(false);

  async function changePassword() {
    pwMsg = ''; pwErr = '';
    if (nw.length < 6) { pwErr = 'New password must be at least 6 characters.'; return; }
    if (nw !== confirm2) { pwErr = 'New passwords do not match.'; return; }
    pwBusy = true;
    try {
      await api.changeMyPassword(cur, nw);
      pwMsg = 'Password changed.';
      cur = ''; nw = ''; confirm2 = '';
    } catch (e) {
      pwErr = e.message;
    } finally {
      pwBusy = false;
    }
  }

  // --- admin: manage users ---
  let users = $state([]);
  let usersErr = $state('');
  let adminMsg = $state('');

  async function loadUsers() {
    if (!me.is_admin) return;
    usersErr = '';
    try {
      const r = await api.listUsers();
      users = r.users;
    } catch (e) {
      usersErr = e.message;
    }
  }
  $effect(() => { loadUsers(); });

  // New-user form.
  let nuName = $state('');
  let nuPass = $state('');
  let nuEmail = $state('');
  let nuAdmin = $state(false);
  let nuBusy = $state(false);

  async function addUser() {
    adminMsg = ''; usersErr = '';
    nuBusy = true;
    try {
      await api.createUser(nuName.trim(), nuPass, nuEmail.trim() || null, nuAdmin);
      adminMsg = `Created ${nuName.trim()}.`;
      nuName = ''; nuPass = ''; nuEmail = ''; nuAdmin = false;
      await loadUsers();
    } catch (e) {
      usersErr = e.message;
    } finally {
      nuBusy = false;
    }
  }

  async function toggleAdmin(u) {
    adminMsg = ''; usersErr = '';
    try {
      await api.updateUser(u.id, { is_admin: !u.is_admin });
      await loadUsers();
    } catch (e) { usersErr = e.message; }
  }

  async function resetPassword(u) {
    const p = prompt(`New password for ${u.username} (min 6 chars):`);
    if (p == null) return;
    adminMsg = ''; usersErr = '';
    try {
      await api.setUserPassword(u.id, p);
      adminMsg = `Password reset for ${u.username}.`;
    } catch (e) { usersErr = e.message; }
  }

  async function removeUser(u) {
    const extra = u.devices > 0 ? `\n\nTheir ${u.devices} device(s) will be unclaimed (not deleted).` : '';
    if (!confirm(`Delete user ${u.username}?${extra}`)) return;
    adminMsg = ''; usersErr = '';
    try {
      await api.deleteUser(u.id);
      adminMsg = `Deleted ${u.username}.`;
      await loadUsers();
    } catch (e) { usersErr = e.message; }
  }
</script>

<div class="page">
  <div class="bar"><h2>Account</h2><span class="muted">{me.username ?? ''}</span></div>

  <div class="card">
    <h3>Change your password</h3>
    <label>Current password<input type="password" bind:value={cur} autocomplete="current-password" /></label>
    <label>New password<input type="password" bind:value={nw} autocomplete="new-password" /></label>
    <label>Confirm new password<input type="password" bind:value={confirm2} autocomplete="new-password" /></label>
    <div class="actions">
      <button onclick={changePassword} disabled={pwBusy || !cur || !nw}>Change password</button>
      {#if pwMsg}<span class="ok">{pwMsg}</span>{/if}
      {#if pwErr}<span class="error">{pwErr}</span>{/if}
    </div>
  </div>

  {#if me.is_admin}
    <div class="card">
      <h3>Manage users</h3>
      {#if usersErr}<p class="error">{usersErr}</p>{/if}

      <table class="users">
        <thead>
          <tr><th>User</th><th>Email</th><th>Role</th><th>Devices</th><th></th></tr>
        </thead>
        <tbody>
          {#each users as u (u.id)}
            <tr>
              <td>{u.username}{#if u.self}<span class="muted"> (you)</span>{/if}</td>
              <td class="muted">{u.email ?? '—'}</td>
              <td>{#if u.is_admin}<span class="badge">admin</span>{:else}<span class="muted">user</span>{/if}</td>
              <td class="muted">{u.devices}</td>
              <td class="row-actions">
                {#if !u.self}
                  <button class="ghost xs" onclick={() => toggleAdmin(u)}>{u.is_admin ? 'Remove admin' : 'Make admin'}</button>
                  <button class="ghost xs" onclick={() => resetPassword(u)}>Reset password</button>
                  <button class="ghost xs danger" onclick={() => removeUser(u)}>Delete</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>

      <h4>Add a user</h4>
      <div class="addrow">
        <input placeholder="username" bind:value={nuName} autocomplete="off" />
        <input placeholder="password (min 6)" type="password" bind:value={nuPass} autocomplete="new-password" />
        <input placeholder="email (optional)" bind:value={nuEmail} autocomplete="off" />
        <label class="toggle"><input type="checkbox" bind:checked={nuAdmin} /> admin</label>
        <button onclick={addUser} disabled={nuBusy || !nuName.trim() || nuPass.length < 6}>Add</button>
      </div>
      {#if adminMsg}<p class="ok">{adminMsg}</p>{/if}
    </div>
  {/if}
</div>

<style>
  .page { padding: 18px; max-width: 680px; margin: 0 auto; }
  .bar { display: flex; align-items: center; gap: 14px; margin-bottom: 16px; }
  h2 { margin: 0; }
  h3 { margin: 0 0 10px; font-size: 14px; }
  h4 { margin: 18px 0 6px; font-size: 13px; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; margin-bottom: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--muted); margin-bottom: 12px; }
  label.toggle { flex-direction: row; align-items: center; gap: 8px; margin-bottom: 0; color: var(--text); white-space: nowrap; }
  label.toggle input { width: 18px; height: 18px; }
  .actions { display: flex; gap: 10px; margin-top: 6px; align-items: center; flex-wrap: wrap; }
  .ok { color: #3fb950; }
  table.users { width: 100%; border-collapse: collapse; font-size: 13px; }
  table.users th { text-align: left; color: var(--muted); font-weight: 500; padding: 6px 8px; border-bottom: 1px solid var(--border); }
  table.users td { padding: 8px; border-bottom: 1px solid var(--border); vertical-align: middle; }
  .row-actions { display: flex; gap: 6px; justify-content: flex-end; flex-wrap: wrap; }
  .badge { background: var(--accent); color: #fff; border-radius: 4px; padding: 1px 7px; font-size: 12px; }
  button.xs { padding: 4px 8px; font-size: 12px; }
  button.danger { border-color: #f85149; color: #f85149; }
  .addrow { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .addrow input { width: auto; flex: 1 1 140px; }
</style>
