/** Public signaling follows the deployed API origin when no separate Calling origin is configured. */
export function callingOrigin(configured = '', api = '', production = false) {
  if (configured || api) return new URL(configured || api).origin;
  return production ? 'https://api.cfmeet.com' : 'http://localhost:8090';
}
