<script>
  // openpilot model overlay on the road (qcamera) video. Projects modelV2 points
  // (device frame: x fwd, y right, z down) to qcamera pixels via the calibrated
  // intrinsics + per-drive rpy, then onto the displayed (object-fit:contain) frame.
  // The camera geometry is fixed, so one saved calibration works for every drive.
  let { frames = [], rpy = [0, 0, 0], curT = 0, calib = null } = $props();

  const QW = 526, QH = 330; // qcamera native pixels
  let canvas;

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

    // qcamera px → displayed px (object-fit: contain against QWxQH).
    const s = Math.min(w / QW, h / QH);
    const ox = (w - QW * s) / 2, oy = (h - QH * s) / 2;
    const R = rot((rpy[0] || 0) + (c.roll || 0), (rpy[1] || 0) + (c.pitch || 0), (rpy[2] || 0) + (c.yaw || 0));

    // Project a device/calib-frame point → [displayX, displayY] or null if behind.
    const proj = (x, y, z) => {
      const dx = R[0][0] * x + R[0][1] * y + R[0][2] * z;
      const dy = R[1][0] * x + R[1][1] * y + R[1][2] * z;
      const dz = R[2][0] * x + R[2][1] * y + R[2][2] * z;
      const vx = dy, vy = dz, vz = dx; // view_frame_from_device_frame
      if (vz <= 1) return null;
      const u = c.fx * (vx / vz) + c.cx;
      const v = c.fy * (vy / vz) + c.cy;
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
      const left = [], right = [];
      for (let i = 0; i < p.x.length; i++) {
        const z = (p.z?.[i] ?? 0) + ph;
        const l = proj(p.x[i], p.y[i] - HALF, z);
        const r = proj(p.x[i], p.y[i] + HALF, z);
        if (l) left.push(l);
        if (r) right.push(r);
      }
      if (left.length > 1 && right.length > 1) {
        ctx.beginPath();
        ctx.moveTo(left[0][0], left[0][1]);
        for (const q of left) ctx.lineTo(q[0], q[1]);
        for (let i = right.length - 1; i >= 0; i--) ctx.lineTo(right[i][0], right[i][1]);
        ctx.closePath();
        ctx.fillStyle = 'rgba(64,156,255,0.35)';
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
        ctx.fillText(`${Math.round(frame.lead.x)} m`, lp[0], lp[1] - sz / 2 - 4);
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
