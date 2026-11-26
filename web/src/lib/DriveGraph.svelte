<script>
  // Speed-over-time graph with openpilot-engaged shading, hard accel/brake marks,
  // and a playhead. Click to seek. Reads the telemetry already loaded by Drive.
  let { telemetry = [], power = [], curT = 0, onseek } = $props();

  const W = 1000, H = 100, PAD = 8;

  let maxT = $derived(telemetry.length ? telemetry[telemetry.length - 1].t || 1 : 1);
  let maxSpeed = $derived(telemetry.reduce((m, s) => Math.max(m, s.speed || 0), 1));

  let speedPath = $derived.by(() => {
    if (telemetry.length < 2) return '';
    let d = '';
    for (let i = 0; i < telemetry.length; i++) {
      const x = (telemetry[i].t / maxT) * W;
      const y = H - PAD - ((telemetry[i].speed || 0) / maxSpeed) * (H - 2 * PAD);
      d += `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)} `;
    }
    return d;
  });

  // Contiguous engaged runs → shaded rects.
  let engaged = $derived.by(() => {
    const out = [];
    let start = null;
    for (let i = 0; i < telemetry.length; i++) {
      const e = telemetry[i].engaged;
      if (e && start === null) start = telemetry[i].t;
      if (start !== null && (!e || i === telemetry.length - 1)) {
        const end = telemetry[i].t;
        out.push({ x: (start / maxT) * W, w: Math.max(1.2, ((end - start) / maxT) * W) });
        start = null;
      }
    }
    return out;
  });

  // Hard accel/brake from the speed derivative (same thresholds as the backend).
  let marks = $derived.by(() => {
    const out = [];
    const MS = 0.44704;
    let pv = null, inB = false, inA = false;
    for (let i = 0; i < telemetry.length; i++) {
      const v = (telemetry[i].speed || 0) * MS;
      if (i > 0 && pv !== null) {
        const dt = telemetry[i].t - telemetry[i - 1].t;
        if (dt > 0 && dt < 2) {
          const a = (v - pv) / dt;
          if (a <= -3.0) { if (!inB) { out.push({ x: (telemetry[i].t / maxT) * W, kind: 'brake' }); inB = true; } } else inB = false;
          if (a >= 2.5) { if (!inA) { out.push({ x: (telemetry[i].t / maxT) * W, kind: 'accel' }); inA = true; } } else inA = false;
        }
      }
      pv = v;
    }
    return out;
  });

  // Driver-monitoring: was DM active at all this drive, and contiguous distracted
  // runs → a band along the bottom.
  let hasDM = $derived(telemetry.some((s) => (s.dm_aware ?? -1) >= 0));
  let distracted = $derived.by(() => {
    const out = [];
    let start = null;
    for (let i = 0; i < telemetry.length; i++) {
      const d = telemetry[i].dm_distracted;
      if (d && start === null) start = telemetry[i].t;
      if (start !== null && (!d || i === telemetry.length - 1)) {
        out.push({ x: (start / maxT) * W, w: Math.max(1.2, ((telemetry[i].t - start) / maxT) * W) });
        start = null;
      }
    }
    return out;
  });

  // Battery power as an area around the midline: discharge up, regen down,
  // scaled to the drive's peak |kW|.
  let peakKw = $derived(power.reduce((m, p) => Math.max(m, Math.abs(p.kw)), 1));
  let maxKw = $derived(power.reduce((m, p) => Math.max(m, p.kw), 0));
  let powerPaths = $derived.by(() => {
    if (power.length < 2) return { out: '', regen: '' };
    const mid = H / 2, half = H / 2 - PAD;
    const area = (sel) => {
      let d = '';
      let open = false;
      for (let i = 0; i < power.length; i++) {
        const p = power[i];
        const gap = i > 0 && p.t - power[i - 1].t > 2;
        const x = ((p.t / maxT) * W).toFixed(1);
        if (open && gap) { d += `L${((power[i - 1].t / maxT) * W).toFixed(1)},${mid} Z `; open = false; }
        const v = sel(p.kw);
        const y = (mid - (v / peakKw) * half).toFixed(1);
        if (!open) { d += `M${x},${mid} `; open = true; }
        d += `L${x},${y} `;
      }
      if (open) d += `L${((power[power.length - 1].t / maxT) * W).toFixed(1)},${mid} Z`;
      return d;
    };
    return { out: area((k) => Math.max(0, k)), regen: area((k) => Math.min(0, k)) };
  });

  let playX = $derived((Math.min(curT, maxT) / maxT) * W);

  function click(e) {
    const r = e.currentTarget.getBoundingClientRect();
    const frac = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width));
    onseek?.(frac * maxT);
  }
</script>

{#if telemetry.length > 1}
  <div class="graph">
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" onclick={click} role="presentation">
      {#each engaged as g}<rect class="eng" x={g.x} y="0" width={g.w} height={H} />{/each}
      {#if power.length > 1}
        <path class="pout" d={powerPaths.out} />
        <path class="pregen" d={powerPaths.regen} />
      {/if}
      <path class="spd" d={speedPath} />
      {#each marks as m}<line class={m.kind} x1={m.x} x2={m.x} y1="0" y2={H} />{/each}
      {#each distracted as d}<rect class="distr" x={d.x} y={H - 6} width={d.w} height="6" />{/each}
      <line class="play" x1={playX} x2={playX} y1="0" y2={H} />
    </svg>
    <div class="legend muted small">
      <span><i class="eng"></i> openpilot</span>
      <span><i class="brake"></i> hard brake</span>
      <span><i class="accel"></i> hard accel</span>
      {#if hasDM}<span><i class="distr"></i> distracted</span>{/if}
      {#if power.length > 1}<span><i class="pout"></i> power</span><span><i class="pregen"></i> regen</span>{/if}
      <span class="right">peak {Math.round(maxSpeed)} mph{#if power.length > 1} · {Math.round(maxKw)} kW{/if}</span>
    </div>
  </div>
{/if}

<style>
  .graph { padding: 6px 0; }
  svg { width: 100%; height: 78px; display: block; cursor: pointer; background: var(--panel-2); border-radius: 6px; }
  .eng { fill: var(--accent); opacity: 0.18; }
  .spd { fill: none; stroke: var(--text); stroke-width: 1.4; vector-effect: non-scaling-stroke; }
  .brake { stroke: #f85149; stroke-width: 1.4; vector-effect: non-scaling-stroke; }
  .accel { stroke: #d29922; stroke-width: 1.4; vector-effect: non-scaling-stroke; }
  .distr { fill: #a371f7; opacity: 0.9; }
  .pout { fill: #d29922; opacity: 0.35; }
  .pregen { fill: #3fb950; opacity: 0.45; }
  .play { stroke: #fff; stroke-width: 1.4; vector-effect: non-scaling-stroke; }
  .legend { display: flex; gap: 12px; align-items: center; margin-top: 4px; }
  .legend .right { margin-left: auto; }
  .legend i { display: inline-block; width: 10px; height: 10px; border-radius: 2px; vertical-align: -1px; margin-right: 3px; }
  .legend i.eng { background: var(--accent); opacity: 0.5; }
  .legend i.brake { background: #f85149; }
  .legend i.accel { background: #d29922; }
  .legend i.distr { background: #a371f7; }
  .legend i.pout { background: #d29922; opacity: 0.6; }
  .legend i.pregen { background: #3fb950; opacity: 0.7; }
</style>
