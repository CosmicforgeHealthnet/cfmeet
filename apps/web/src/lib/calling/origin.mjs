/** Public signaling follows the deployed API origin when no separate Calling origin is configured. */
export function callingOrigin(configured = '', api = '', production = false) {
  if (configured || api) return new URL(configured || api).origin;
  return production ? 'https://api.cfmeet.com' : 'http://localhost:8090';
}

/**
 * The signaling WebSocket URL (CAL-020). A production build only ever opens wss:// — a
 * plain-http Calling origin is refused instead of downgrading the bearer subprotocol to ws://.
 * @param {string} signalingPath
 * @param {string} origin
 * @param {boolean} [production]
 * @returns {URL}
 */
export function signalingSocketUrl(signalingPath, origin, production = false) {
  const url = new URL(signalingPath, origin);
  if (url.origin !== new URL(origin).origin) throw new Error('Signaling must stay on the Calling origin.');
  if (url.protocol === 'https:') url.protocol = 'wss:';
  else if (url.protocol === 'http:' && !production) url.protocol = 'ws:';
  else throw new Error('Calling signaling is not securely configured.');
  return url;
}
