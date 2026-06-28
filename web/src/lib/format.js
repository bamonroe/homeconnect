// Small presentation helpers shared across the Drive/Stats/Settings/Queues views,
// so the speed-color ramp, data-type labels, and the nearest-sample lookup live
// in one place instead of being copy-pasted per component.

// Speed → color ramp (blue ≈ slow → green ≈ fast), with a tunable alpha. Returns
// hsla so callers wanting opacity pass `a`; the default a=1 reads like plain hsl.
export function speedColor(mph, a = 1) {
  const t = Math.max(0, Math.min(1, mph / 70));
  return `hsla(${Math.round(210 - 90 * t)}, 80%, 55%, ${a})`;
}

// Human labels for the per-drive data types (camera/log ids).
export const TYPE_LABELS = {
  qcamera: 'Road (qcamera)',
  fcamera: 'Road HD (fcamera)',
  dcamera: 'Driver (dcamera)',
  ecamera: 'Wide (ecamera)',
  rlog: 'Raw log (rlog)',
  qlog: 'Driving log (qlog)',
};

// Short labels for on-disk segment filenames (used in the sync queue list).
const FILE_LABELS = {
  'qcamera.ts': 'Road', 'fcamera.hevc': 'Road HD', 'dcamera.hevc': 'Driver',
  'ecamera.hevc': 'Wide', 'qlog.zst': 'Log', 'qlog.bz2': 'Log',
  'rlog.zst': 'Raw log', 'rlog.bz2': 'Raw log',
};
export const fileLabel = (f) => FILE_LABELS[f] ?? f;

// Copy text to the clipboard, returning whether it succeeded. The async
// Clipboard API only exists in a secure context (HTTPS/localhost); this site is
// served over plain HTTP on the tailnet, so fall back to a temporary-textarea
// `execCommand('copy')`, which works over http://.
export async function copyText(text) {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {}
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.left = '-9999px';
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand('copy');
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

// Binary-search a time-sorted array for the last sample at or before `t`, returning
// that element (or the first sample if `t` precedes them all, null if empty). Every
// `{ t, … }` series (coords, telemetry, model frames) is synced to the video clock
// this way.
export function findNearest(arr, t) {
  if (!arr || !arr.length) return null;
  let lo = 0, hi = arr.length - 1, best = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid].t <= t) { best = mid; lo = mid + 1; }
    else hi = mid - 1;
  }
  return arr[best];
}
