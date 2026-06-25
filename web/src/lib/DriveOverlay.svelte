<script>
  // openpilot model overlay on the road (qcamera) video. Projects modelV2 points
  // (device frame: x fwd, y right, z down) to qcamera pixels via the calibrated
  // intrinsics + per-drive rpy, then onto the displayed (object-fit:contain) frame.
  // The camera geometry is fixed, so one saved calibration works for every drive.
  let { frames = [], rpy = [0, 0, 0], curT = 0, calib = null, fisheye = false, nw = 526, nh = 330 } = $props();
  let canvas;

  // speed → color (blue slow → green fast), tunable alpha.
  function speedColor(mph, a = 1) {
    const t = Math.max(0, Math.min(1, mph / 70));
    return `hsla(${Math.round(210 - 90 * t)}, 80%, 55%, ${a})`;
  }

  // device_from_calib rotation (Rz(yaw)·Ry(pitch)·Rx(roll)); rpy + saved offsets.
  function rot(r, p, y) {
    const cr = Math.cos(r), sr = Math.sin(r), cp = Math.cos(p), sp = Math.sin(p), cy = Math.cos(y), sy = Math.sin(y);
    return [
      [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
      [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
      [-sp, cp * sr, cp * cr],
    ];
  }

  let frame = $derived.by(() => {
    if (!frames.length) return null;
    let lo = 0, hi = frames.length - 1, best = 0;
    while (lo <= hi) { const m = (lo + hi) >> 1; if (frames[m].t <= curT) { best = m; lo = m + 1; } else hi = m - 1; }
    return frames[best];
  });

  function draw() {
    const cv = canvas;
    if (!cv) return;
    const ctx = cv.getContext('2d');
    const w = cv.clientWidth, h = cv.clientHeight;
    if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; }
    ctx.clearRect(0, 0, w, h);
    const c = calib;
    if (!frame || !c) return;

    // native camera px → displayed px (object-fit: contain against nw×nh).
    const s = Math.min(w / nw, h / nh);
    const ox = (w - nw * s) / 2, oy = (h - nh * s) / 2;
    const R = rot((rpy[0] || 0) + (c.roll || 0), (rpy[1] || 0) + (c.pitch || 0), (rpy[2] || 0) + (c.yaw || 0));

    // Project a device/calib-frame point → [displayX, displayY] or null if behind.
    const proj = (x, y, z) => {
      const dx = R[0][0] * x + R[0][1] * y + R[0][2] * z;
      const dy = R[1][0] * x + R[1][1] * y + R[1][2] * z;
      const dz = R[2][0] * x + R[2][1] * y + R[2][2] * z;
      const vx = dy, vy = dz, vz = dx; // view_frame_from_device_frame
      if (vz <= 1) return null;
      let u, v;
      if (fisheye) {
        // equidistant fisheye: r = f·θ from the optical axis (z forward)
        const theta = Math.atan2(Math.hypot(vx, vy), vz);
        const ang = Math.atan2(vy, vx);
        const r = c.fx * theta;
        u = c.cx + r * Math.cos(ang);
        v = c.cy + r * Math.sin(ang);
      } else {
        u = c.fx * (vx / vz) + c.cx;
        v = c.fy * (vy / vz) + c.cy;
      }
      return [ox + u * s, oy + v * s];
    };
    const projLine = (xy, n) => {
      const pts = [];
      for (let i = 0; i < (xy.x?.length || 0); i++) {
        const p = proj(xy.x[i], xy.y[i], xy.z?.[i] ?? 0);
        if (p) pts.push(p);
      }
      return pts;
    };
    const stroke = (pts, color, width) => {
      if (pts.length < 2) return;
      ctx.beginPath();
      ctx.moveTo(pts[0][0], pts[0][1]);
      for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i][0], pts[i][1]);
      ctx.strokeStyle = color; ctx.lineWidth = width; ctx.lineJoin = 'round';
      ctx.stroke();
    };

    // road edges (dim), lane lines (white), path corridor (green fill), lead box.
    for (const e of frame.edges) stroke(projLine(e), 'rgba(150,160,170,0.55)', 2);
    for (const l of frame.lanes) stroke(projLine(l), 'rgba(230,237,243,0.85)', 2);

    // The path is the car's trajectory at ~camera height; add `h` so it lands on
    // the road (lanes/edges already carry the road-surface z, so they're unshifted).
    const ph = c.h ?? 1.2;
    const p = frame.path;
    if (p?.x?.length > 1) {
      const HALF = 0.9;
      const L = [], Rr = [];
      for (let i = 0; i < p.x.length; i++) {
        const z = (p.z?.[i] ?? 0) + ph;
        L.push(proj(p.x[i], p.y[i] - HALF, z));
        Rr.push(proj(p.x[i], p.y[i] + HALF, z));
      }
      // Per-segment quads colored by predicted speed.
      for (let i = 0; i < p.x.length - 1; i++) {
        const a = L[i], b = Rr[i], c2 = Rr[i + 1], d2 = L[i + 1];
        if (!a || !b || !c2 || !d2) continue;
        const mph = (frame.speed?.[i] ?? 0) * 2.237;
        ctx.beginPath();
        ctx.moveTo(a[0], a[1]); ctx.lineTo(b[0], b[1]); ctx.lineTo(c2[0], c2[1]); ctx.lineTo(d2[0], d2[1]);
        ctx.closePath();
        ctx.fillStyle = speedColor(mph, 0.42);
        ctx.fill();
      }
    }

    if (frame.lead) {
      const lp = proj(frame.lead.x, frame.lead.y, ph);
      if (lp) {
        const sz = Math.max(10, 1400 / Math.max(5, frame.lead.x));
        ctx.strokeStyle = '#f0883e'; ctx.lineWidth = 3;
        ctx.strokeRect(lp[0] - sz / 2, lp[1] - sz / 2, sz, sz);
        ctx.fillStyle = '#f0b429'; ctx.font = '13px sans-serif'; ctx.textAlign = 'center';
        ctx.fillText(`${Math.round(frame.lead.x)} m · ${Math.round(frame.lead.v * 2.237)} mph`, lp[0], lp[1] - sz / 2 - 4);
      }
    }
  }

  $effect(() => {
    // Redraw on time/calib/frame change. (curT/frame/calib are read in draw.)
    frame; curT; calib;
    draw();
  });
</script>

<canvas bind:this={canvas} class="overlay"></canvas>

<style>
  .overlay { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; }
</style>
