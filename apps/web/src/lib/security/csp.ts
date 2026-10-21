/**
 * Browser security policy of the CF Meet web app (Part V remediation M10).
 *
 * - Scripts: only this origin's bundles and inline scripts carrying the per-request
 *   nonce (`'strict-dynamic'` lets those load their own chunks). No `'unsafe-inline'`,
 *   and no `'unsafe-eval'` outside `next dev` (React's development tooling needs it).
 * - Trusted Types: DOM XSS sinks (`innerHTML`, `script.src`, ...) accept only values from
 *   named policies; untyped strings are refused by the browser.
 * - No plugins, no `<base>` hijacking, no framing (clickjacking), forms post to this
 *   origin only.
 * - Network: this origin, the configured API origin and WebSocket media / messaging
 *   endpoints (LiveKit and the Messaging realtime gateway are deployment-specific);
 *   `CSP_EXTRA_CONNECT_SRC` adds origins explicitly.
 * - Styles allow inline styles: server-rendered `style` attributes and component
 *   libraries need them; style injection cannot run script under this policy.
 */

export type CspOptions = {
  nonce: string;
  dev: boolean;
  /** The document was requested over HTTPS (directly or via the TLS-terminating proxy). */
  secure?: boolean;
  apiUrl?: string;
  extraConnectSrc?: string;
  /** Messaging realtime gateway origin (ticket POST + WebSocket). */
  messagingUrl?: string;
  /** Object storage origin of signed attachment uploads / downloads. */
  storageUrl?: string;
};

/** Trusted Types policy names the app and Next.js are allowed to create. */
export const TRUSTED_TYPES_POLICIES = ["nextjs", "nextjs#bundler", "cfmeet", "default"];

function origin(url: string | undefined): string | null {
  if (!url) return null;
  try {
    return new URL(url).origin;
  } catch {
    return null;
  }
}

const LOOPBACK = ["localhost", "127.0.0.1", "[::1]"];

/**
 * A deployment-configured service origin and, for the realtime gateway, its WebSocket
 * form. HTTPS everywhere; plain HTTP / WS only for a loopback host (local runs, CI).
 */
function serviceOrigins(url: string | undefined, websocket: boolean): string[] {
  if (!url) return [];
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return [];
  }
  const loopback = LOOPBACK.includes(u.hostname);
  if (u.protocol !== "https:" && !(loopback && u.protocol === "http:")) return [];
  const out = [u.origin];
  if (websocket) out.push(`${u.protocol === "https:" ? "wss:" : "ws:"}//${u.host}`);
  return out;
}

export function buildCsp(o: CspOptions): string {
  if (!/^[A-Za-z0-9+/=_-]{16,}$/.test(o.nonce)) {
    throw new Error("a CSP nonce must be a random base64 value");
  }
  const api = origin(o.apiUrl);
  const connect = ["'self'", api, "wss:", "https://*.livekit.cloud"]
    .concat(serviceOrigins(o.messagingUrl, true), serviceOrigins(o.storageUrl, false))
    .concat((o.extraConnectSrc ?? "").split(/\s+/).filter((s) => /^(https|wss):\/\//.test(s)))
    .filter((s): s is string => Boolean(s));
  const directives: Record<string, string[]> = {
    "default-src": ["'self'"],
    "script-src": ["'self'", `'nonce-${o.nonce}'`, "'strict-dynamic'"]
      .concat(o.dev ? ["'unsafe-eval'"] : []),
    "style-src": ["'self'", "'unsafe-inline'"],
    "img-src": ["'self'", "data:", "blob:", "https://i.pravatar.cc"].concat(api ? [api] : []),
    "font-src": ["'self'", "data:"],
    "media-src": ["'self'", "blob:", "mediastream:"],
    "connect-src": o.dev ? connect.concat(["ws:"]) : connect,
    "worker-src": ["'self'", "blob:"],
    "object-src": ["'none'"],
    "base-uri": ["'none'"],
    "form-action": ["'self'"],
    "frame-ancestors": ["'none'"],
    "require-trusted-types-for": ["'script'"],
    "trusted-types": TRUSTED_TYPES_POLICIES.concat(["'allow-duplicates'"]),
  };
  if (!o.dev && o.secure) directives["upgrade-insecure-requests"] = [];
  return Object.entries(directives)
    .map(([k, v]) => (v.length ? `${k} ${v.join(" ")}` : k))
    .join("; ");
}

/** Where the shipped Messaging crypto worker, its glue and its module are served. */
export const MESSAGING_CRYPTO_PATH = "/messaging-crypto";

/**
 * The crypto worker's OWN policy (a dedicated worker uses the CSP of its script's
 * response). The only grant of WebAssembly compilation in the app: pages never get
 * 'wasm-unsafe-eval'. The worker may import its same-origin glue and fetch its module,
 * nothing else: no network, no nested workers, no JavaScript eval.
 */
export const MESSAGING_CRYPTO_WORKER_CSP = [
  "default-src 'none'",
  "script-src 'self' 'wasm-unsafe-eval'",
  "connect-src 'self'",
  "worker-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
].join("; ");

/** Static headers applied to every response (next.config.ts). */
export const SECURITY_HEADERS: { key: string; value: string }[] = [
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  { key: "Cross-Origin-Opener-Policy", value: "same-origin" },
  {
    key: "Permissions-Policy",
    value:
      "camera=(self), microphone=(self), display-capture=(self), fullscreen=(self), geolocation=(), payment=(), usb=(), serial=(), hid=(), bluetooth=(), interest-cohort=()",
  },
  { key: "Strict-Transport-Security", value: "max-age=63072000; includeSubDomains" },
];
