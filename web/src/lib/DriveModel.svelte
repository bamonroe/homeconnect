<script>
  // Top-down "what openpilot saw": predicted path, lane lines, road edges, and
  // lead car, from modelV2 (device frame: x = forward, y = left). Synced to curT.
  let { frames = [], curT = 0 } = $props();

  const W = 280, H = 360;
  const FWD = 90; // meters of forward range shown
  const LAT = 18; // meters left/right shown
  const cx = W / 2;
  const sx = (y) => cx - (y / LAT) * cx; // +y is left → screen left
  const sy = (x) => H - (Math.max(0, x) / FWD) * H; // forward → up
  const poly = (xy) => {
    if (!xy || !xy.x) return '';
    let d = '';
    for (let i = 0; i < xy.x.length; i++) d += `${i ? 'L' : 'M'}${sx(xy.y[i]).toFixed(1)},${sy(xy.x[i]).toFixed(1)} `;
    return d;
  };

  // Nearest frame at/just-before curT.
  let frame = $derived.by(() => {
    if (!frames.length) return null;
    let lo = 0, hi = frames.length - 1, best = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (frames[mid].t <= curT) { best = mid; lo = mid + 1; } else hi = mid - 1;
    }
    return frames[best];
  });

  // Filled corridor between the model path's left/right (approximate width).
  let pathArea = $derived.by(() => {
    const p = frame?.path;
    if (!p || !p.x || p.x.length < 2) return '';
    const half = 0.9; // ~lane half-width for the drawn corridor
    let l = '', r = '';
    for (let i = 0; i < p.x.length; i++) {
      l += `${i ? 'L' : 'M'}${sx(p.y[i] + half).toFixed(1)},${sy(p.x[i]).toFixed(1)} `;
    }
    for (let i = p.x.length - 1; i >= 0; i--) {
      r += `L${sx(p.y[i] - half).toFixed(1)},${sy(p.x[i]).toFixed(1)} `;
    }
    return l + r + 'Z';
  });
</script>

<div class="model">
  <svg viewBox="0 0 {W} {H}" preserveAspectRatio="xMidYMid meet">
    <!-- distance rings -->
    {#each [25, 50, 75] as d}
      <line class="ring" x1="0" x2={W} y1={sy(d)} y2={sy(d)} />
      <text class="ringlbl" x="4" y={sy(d) - 3}>{d}m</text>
    {/each}
    {#if frame}
      {#each frame.edges as e}<path class="edge" d={poly(e)} />{/each}
      {#each frame.lanes as l}<path class="lane" d={poly(l)} />{/each}
      <path class="corridor" d={pathArea} />
      <path class="path" d={poly(frame.path)} />
      {#if frame.lead}
        <rect class="lead" x={sx(frame.lead.y) - 9} y={sy(frame.lead.x) - 7} width="18" height="14" rx="2" />
        <text class="leadlbl" x={sx(frame.lead.y)} y={sy(frame.lead.x) - 11} text-anchor="middle">{Math.round(frame.lead.x)}m</text>
      {/if}
    {/if}
    <!-- ego -->
    <polygon class="ego" points="{cx - 7},{H - 4} {cx + 7},{H - 4} {cx},{H - 18}" />
  </svg>
</div>

<style>
  .model { padding: 6px 0; }
  svg { width: 100%; height: 300px; display: block; background: #11151c; border-radius: 6px; }
  .ring { stroke: #2a3038; stroke-width: 1; }
  .ringlbl { fill: #5a6472; font-size: 9px; }
  .edge { fill: none; stroke: #6e7681; stroke-width: 2; opacity: 0.7; }
  .lane { fill: none; stroke: #e6edf3; stroke-width: 2; opacity: 0.85; }
  .corridor { fill: var(--accent); opacity: 0.18; stroke: none; }
  .path { fill: none; stroke: var(--accent); stroke-width: 2.5; }
  .lead { fill: #f0883e; opacity: 0.9; }
  .leadlbl { fill: #f0b429; font-size: 10px; }
  .ego { fill: #3fb950; }
</style>
