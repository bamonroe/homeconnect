<script>
  // Top-down "what openpilot saw": path (colored by predicted speed), lane lines,
  // road edges, lead car — from modelV2 (device frame: x fwd, y right, z down).
  let { frames = [], curT = 0 } = $props();

  const W = 280, H = 360;
  let fwd = $state(Number(localStorage.getItem('hc_td_fwd')) || 90); // meters ahead shown
  const cx = W / 2;
  // True-scale top-down: same px/m horizontally and vertically.
  let scale = $derived(H / fwd);
  const sy = (x) => H - Math.max(0, x) * (H / fwd);
  const sx = (y) => cx + y * (H / fwd); // device +y is right → screen right

  function setFwd(v) { fwd = v; localStorage.setItem('hc_td_fwd', String(v)); }
  function speedColor(mph) {
    const t = Math.max(0, Math.min(1, mph / 70));
    return `hsl(${Math.round(210 - 90 * t)}, 80%, 55%)`;
  }
  const polyD = (xy) => {
    if (!xy?.x) return '';
    let d = '';
    for (let i = 0; i < xy.x.length; i++) d += `${i ? 'L' : 'M'}${sx(xy.y[i]).toFixed(1)},${sy(xy.x[i]).toFixed(1)} `;
    return d;
  };

  let frame = $derived.by(() => {
    if (!frames.length) return null;
    let lo = 0, hi = frames.length - 1, best = 0;
    while (lo <= hi) { const m = (lo + hi) >> 1; if (frames[m].t <= curT) { best = m; lo = m + 1; } else hi = m - 1; }
    return frames[best];
  });

  // Path as speed-colored segments.
  let pathSegs = $derived.by(() => {
    const p = frame?.path;
    if (!p?.x || p.x.length < 2) return [];
    const segs = [];
    for (let i = 0; i < p.x.length - 1; i++) {
      segs.push({
        d: `M${sx(p.y[i]).toFixed(1)},${sy(p.x[i]).toFixed(1)} L${sx(p.y[i + 1]).toFixed(1)},${sy(p.x[i + 1]).toFixed(1)}`,
        c: speedColor((frame.speed?.[i] ?? 0) * 2.237),
      });
    }
    return segs;
  });
  let egoMph = $derived(frame?.speed?.length ? Math.round(frame.speed[0] * 2.237) : null);
</script>

<div class="model">
  <svg viewBox="0 0 {W} {H}" preserveAspectRatio="xMidYMid meet">
    {#each [25, 50, 75, 100, 150, 200].filter((d) => d < fwd) as d}
      <line class="ring" x1="0" x2={W} y1={sy(d)} y2={sy(d)} />
      <text class="ringlbl" x="4" y={sy(d) - 3}>{d}m</text>
    {/each}
    {#if frame}
      {#each frame.edges as e}<path class="edge" d={polyD(e)} />{/each}
      {#each frame.lanes as l}<path class="lane" d={polyD(l)} />{/each}
      {#each pathSegs as s}<path d={s.d} stroke={s.c} stroke-width="6" fill="none" stroke-linecap="round" opacity="0.95" />{/each}
      {#if frame.lead}
        <rect class="lead" x={sx(frame.lead.y) - 9} y={sy(frame.lead.x) - 7} width="18" height="14" rx="2" />
        <text class="leadlbl" x={sx(frame.lead.y)} y={sy(frame.lead.x) - 11} text-anchor="middle">{Math.round(frame.lead.x)}m · {Math.round(frame.lead.v * 2.237)}mph</text>
      {/if}
    {/if}
    <polygon class="ego" points="{cx - 7},{H - 4} {cx + 7},{H - 4} {cx},{H - 18}" />
  </svg>
  <div class="td-ctrl">
    {#if egoMph != null}<span class="muted small">{egoMph} mph</span>{/if}
    <span class="zoom">
      {#each [50, 100, 200] as z}
        <button class="ghost zb" class:active={fwd === z} onclick={() => setFwd(z)}>{z}m</button>
      {/each}
    </span>
  </div>
</div>

<style>
  .model { padding: 6px 0; }
  svg { width: 100%; height: 300px; display: block; background: #11151c; border-radius: 6px; }
  .ring { stroke: #2a3038; stroke-width: 1; }
  .ringlbl { fill: #5a6472; font-size: 9px; }
  .edge { fill: none; stroke: #6e7681; stroke-width: 2; opacity: 0.7; }
  .lane { fill: none; stroke: #e6edf3; stroke-width: 2; opacity: 0.85; }
  .lead { fill: #f0883e; opacity: 0.9; }
  .leadlbl { fill: #f0b429; font-size: 10px; }
  .ego { fill: #3fb950; }
  .td-ctrl { display: flex; align-items: center; justify-content: space-between; margin-top: 4px; }
  .zoom { display: flex; gap: 4px; }
  .zb { padding: 2px 8px; font-size: 11px; }
  .zb.active { border-color: var(--accent); color: var(--accent); }
</style>
