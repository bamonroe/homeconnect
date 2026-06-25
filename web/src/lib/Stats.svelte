<script>
  import { api } from './api.js';

  let s = $state(null);
  let error = $state('');

  async function load() {
    error = '';
    try { s = await api.myStats(); } catch (e) { error = e.message; }
  }
  $effect(() => { load(); });

  const n0 = (x) => Math.round(x).toLocaleString();
  const n1 = (x) => (Math.round(x * 10) / 10).toLocaleString();
</script>

<div class="page">
  <div class="bar"><h2>Stats</h2><span class="muted">all-time</span></div>

  {#if error}
    <p class="error">{error}</p>
  {:else if !s}
    <p class="muted">Loading…</p>
  {:else if s.routes === 0}
    <p class="muted">No parsed drives yet.</p>
  {:else}
    <div class="hero">
      <div class="big">{n1(s.autonomy)}<span class="pct">%</span></div>
      <div class="muted">driven by openpilot · {n1(s.engaged_miles)} of {n1(s.miles)} mi</div>
    </div>

    <div class="grid">
      <div class="stat"><div class="v">{n0(s.miles)}</div><div class="k">miles driven</div></div>
      <div class="stat"><div class="v">{n1(s.drive_hours)}</div><div class="k">hours driving</div></div>
      <div class="stat"><div class="v">{n0(s.routes)}</div><div class="k">drives</div></div>
      <div class="stat"><div class="v">{n0(s.disengagements)}</div><div class="k">disengagements</div></div>
      <div class="stat"><div class="v">{n1(s.disengagements_per_100mi)}</div><div class="k">per 100 engaged mi</div></div>
      <div class="stat"><div class="v">{n0(s.avg_speed)}</div><div class="k">avg mph</div></div>
      <div class="stat"><div class="v">{n0(s.max_speed)}</div><div class="k">top mph</div></div>
      <div class="stat"><div class="v">{n0(s.hard_brake)}</div><div class="k">hard brakes</div></div>
      <div class="stat"><div class="v">{n0(s.hard_accel)}</div><div class="k">hard accels</div></div>
    </div>
  {/if}
</div>

<style>
  .page { padding: 18px; max-width: 720px; margin: 0 auto; }
  .bar { display: flex; align-items: baseline; gap: 12px; margin-bottom: 18px; }
  h2 { margin: 0; }
  .hero { background: var(--panel); border: 1px solid var(--border); border-radius: 12px; padding: 26px; text-align: center; margin-bottom: 16px; }
  .big { font-size: 64px; font-weight: 700; line-height: 1; color: var(--accent); }
  .big .pct { font-size: 28px; margin-left: 4px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); gap: 12px; }
  .stat { background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; }
  .stat .v { font-size: 26px; font-weight: 700; }
  .stat .k { color: var(--muted); font-size: 13px; margin-top: 4px; }
</style>
