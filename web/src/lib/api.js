// Tiny same-origin API client. The JWT is kept in localStorage and sent as
// `Authorization: JWT <token>` (the scheme the backend expects).

const TOKEN_KEY = 'hc_token';
const USER_KEY = 'hc_user';

export function getToken() {
  return localStorage.getItem(TOKEN_KEY);
}
export function getUser() {
  try {
    return JSON.parse(localStorage.getItem(USER_KEY) || 'null');
  } catch {
    return null;
  }
}
export function setSession(token, user) {
  localStorage.setItem(TOKEN_KEY, token);
  localStorage.setItem(USER_KEY, JSON.stringify(user));
}
export function clearSession() {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(USER_KEY);
}

async function req(method, path, body) {
  const headers = {};
  const token = getToken();
  if (token) headers['Authorization'] = `JWT ${token}`;
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const res = await fetch(path, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = `${res.status}`;
    try {
      const j = await res.json();
      if (j.error) msg = j.error;
    } catch {}
    const err = new Error(msg);
    err.status = res.status;
    throw err;
  }
  const ct = res.headers.get('content-type') || '';
  return ct.includes('application/json') ? res.json() : res.text();
}

async function form(path, fields) {
  const headers = { 'Content-Type': 'application/x-www-form-urlencoded' };
  const token = getToken();
  if (token) headers['Authorization'] = `JWT ${token}`;
  const res = await fetch(path, { method: 'POST', headers, body: new URLSearchParams(fields) });
  if (!res.ok) {
    let msg = `${res.status}`;
    try {
      const j = await res.json();
      if (j.error) msg = j.error;
    } catch {}
    throw new Error(msg);
  }
  return res.json();
}

export const api = {
  login: (username, password) => req('POST', '/v1/auth/login', { username, password }),
  me: () => req('GET', '/v1/me'),
  devices: () => req('GET', '/v1/me/devices'),
  myStats: () => req('GET', '/v1/me/stats'),
  myPaths: () => req('GET', '/v1/me/paths'),
  unpairedDevices: () => req('GET', '/v1/me/unpaired_devices'),
  claim: (dongle) => req('POST', `/v1/devices/${dongle}/claim`),
  // Secure pair-token flow (the code the device shows). Accepts a raw token or
  // a full `...?pair=<token>` URL.
  pair: (tokenOrUrl) => {
    let tok = tokenOrUrl.trim();
    const m = tok.match(/[?&]pair=([^&]+)/);
    if (m) tok = decodeURIComponent(m[1]);
    return form('/v2/pilotpair', { pair_token: tok });
  },
  routes: (dongle, params = {}) => {
    const qs = new URLSearchParams(params).toString();
    return req('GET', `/v1/devices/${dongle}/routes_segments${qs ? `?${qs}` : ''}`);
  },
  // Media URLs (built so the browser can fetch directly, with the share sig).
  camM3u8: (fullname, cam = 'qcamera') =>
    `/v1/route/${encodeURIComponent(fullname)}/${cam}.m3u8`,
  // Pre-built stitched movies (single MP4 per camera, audio muxed in).
  routeMovies: (fullname) => req('GET', `/v1/route/${encodeURIComponent(fullname)}/movies`),
  movieUrl: (fullname, cam) =>
    `/v1/route/${encodeURIComponent(fullname)}/movie/${cam}.mp4?sig=${getToken()}`,
  // The signed token to append to direct media/artifact fetches.
  sig: () => getToken(),
  // admin: retention
  retention: () => req('GET', '/v1/admin/retention'),
  setRetention: (p) => req('POST', '/v1/admin/retention', p),
  runRetention: () => req('POST', '/v1/admin/retention/run'),
  // admin: transcode device
  transcode: () => req('GET', '/v1/admin/transcode'),
  setTranscode: (device) => req('POST', '/v1/admin/transcode', { device }),
  // road-camera calibration for the model overlay
  camCalib: () => req('GET', '/v1/admin/cam-calib'),
  setCamCalib: (c) => req('POST', '/v1/admin/cam-calib', c),
  // device settings (openpilot params over SSH)
  deviceParams: (dongle) => req('GET', `/v1/devices/${dongle}/params`),
  setDeviceParam: (dongle, key, value) => req('POST', `/v1/devices/${dongle}/params`, { key, value }),
  // admin: automatic-sync on/off toggle + loop interval
  syncSettings: () => req('GET', '/v1/admin/sync'),
  setSync: (patch) => req('POST', '/v1/admin/sync', patch),
  // admin: movie-encoder on/off toggle + sweep interval
  encodingSettings: () => req('GET', '/v1/admin/encoding'),
  setEncoding: (patch) => req('POST', '/v1/admin/encoding', patch),
  // sync queue counter (drives + files queued/in-flight)
  syncQueue: () => req('GET', '/v1/sync/queue'),
  // movie-encoder queue counter (movies left to build + current label)
  movieQueue: () => req('GET', '/v1/movies/queue'),
  // per-drive sync settings (override of the global default)
  routeSync: (fullname) => req('GET', `/v1/route/${encodeURIComponent(fullname)}/sync`),
  setRouteSync: (fullname, patch) => req('POST', `/v1/route/${encodeURIComponent(fullname)}/sync`, patch),
  // device sync (SSH pull). opts: { full: bool, route: '<ts>', types: ['fcamera',…] }
  sync: (dongle, { full = false, route, types } = {}) => {
    const p = new URLSearchParams();
    if (full) p.set('full', 'true');
    if (route) p.set('route', route);
    if (types && types.length) p.set('types', types.join(','));
    const qs = p.toString();
    return req('POST', `/v1/devices/${dongle}/sync${qs ? `?${qs}` : ''}`);
  },
  // manage data
  downloadUrl: (fullname, types) =>
    `/v1/route/${encodeURIComponent(fullname)}/download?types=${types.join(',')}&sig=${getToken()}`,
  // target: 'server' (default), 'device', or 'both'.
  deleteData: (fullname, types, target = 'server') =>
    req('POST', `/v1/route/${encodeURIComponent(fullname)}/delete`, { types, target }),
};
