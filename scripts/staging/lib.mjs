// Shared helpers for the hosted-staging certification kit (scripts/staging).
// Node 22+, standard library only (fetch, WebSocket, node:crypto, node:zlib).
//
// SAFETY: these tools only ever talk to the URLs they are given, and they refuse a target
// that is a known CF Meet production host, a host outside the operator's explicit staging
// allowlist, or plain HTTP to anything but loopback. Credentials are read from files
// (never from the command line, never printed).

import { readFileSync, statSync } from "node:fs";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { deflateSync, crc32 } from "node:zlib";

/** Known production hosts of CF Meet (from the repository's deployment scripts and docs). */
export const PRODUCTION_HOSTS = ["cfmeet.com", "www.cfmeet.com", "api.cfmeet.com"];

export class SafetyError extends Error {}

export function parseArgs(argv, spec) {
  const out = { ...spec.defaults };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) throw new Error(`unexpected argument ${a}`);
    const [k, inline] = a.slice(2).split("=", 2);
    if (spec.flags?.includes(k)) {
      out[k] = true;
      continue;
    }
    if (!(k in spec.defaults)) throw new Error(`unknown option --${k}`);
    out[k] = inline ?? argv[++i];
    if (out[k] === undefined) throw new Error(`--${k} needs a value`);
  }
  return out;
}

function isLoopback(host) {
  return host === "localhost" || host === "::1" || host === "[::1]" || /^127\./.test(host);
}

/**
 * Refuses a target that must never be driven by the kit. `allowedHosts` is the operator's
 * explicit staging allowlist (CFMEET_STAGING_ALLOWED_HOSTS); loopback is always allowed
 * (local rehearsal of the kit against real processes).
 */
export function assertStagingTarget(rawUrl, allowedHosts) {
  let u;
  try {
    u = new URL(rawUrl);
  } catch {
    throw new SafetyError(`not a URL: ${rawUrl}`);
  }
  const host = u.hostname.toLowerCase();
  if (PRODUCTION_HOSTS.includes(host)) {
    throw new SafetyError(`${host} is a CF Meet production host; the staging kit never targets production`);
  }
  for (const a of allowedHosts) {
    if (PRODUCTION_HOSTS.includes(a)) {
      throw new SafetyError(`the staging allowlist contains the production host ${a}`);
    }
  }
  if (isLoopback(host)) return u;
  if (u.protocol !== "https:") throw new SafetyError(`${rawUrl}: a non-loopback target must use https`);
  if (!allowedHosts.includes(host)) {
    throw new SafetyError(`${host} is not in CFMEET_STAGING_ALLOWED_HOSTS (${allowedHosts.join(", ") || "empty"})`);
  }
  return u;
}

export function allowedHostsFromEnv() {
  return (process.env.CFMEET_STAGING_ALLOWED_HOSTS ?? "")
    .split(",")
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
}

/** Reads a secret file; refuses one other users can read (mode & 0o077). */
export function readSecretFile(path, what) {
  if (!path) throw new SafetyError(`${what}: no file given`);
  const st = statSync(path);
  if (process.platform !== "win32" && (st.mode & 0o077) !== 0) {
    throw new SafetyError(`${what}: ${path} is readable by other users (chmod 600)`);
  }
  const v = readFileSync(path, "utf8").trim();
  if (!v) throw new SafetyError(`${what}: ${path} is empty`);
  return v;
}

export function percentiles(samples) {
  const v = [...samples].sort((a, b) => a - b);
  if (v.length === 0) return { samples: 0 };
  const p = (q) => v[Math.round((v.length - 1) * q)];
  const r = (x) => Math.round(x * 100) / 100;
  return { samples: v.length, p50_ms: r(p(0.5)), p95_ms: r(p(0.95)), p99_ms: r(p(0.99)), max_ms: r(v[v.length - 1]) };
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Runs `fn` over `items` with at most `limit` in flight. */
export async function pool(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i], i);
    }
  });
  await Promise.all(workers);
  return out;
}

/** Messaging public API client acting as one external application. */
export class MessagingClient {
  constructor(base, credential, { timeoutMs = 30000 } = {}) {
    this.base = base.replace(/\/$/, "");
    this.credential = credential;
    this.timeoutMs = timeoutMs;
  }

  async call(method, path, { acting, body, key, auth } = {}) {
    const headers = { authorization: auth ?? `Bearer ${this.credential}` };
    if (acting) headers["cf-acting-principal"] = acting;
    if (key) headers["idempotency-key"] = key;
    if (body !== undefined) headers["content-type"] = "application/json";
    const started = performance.now();
    const r = await fetch(`${this.base}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    const text = await r.text();
    let json = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = null;
    }
    return { status: r.status, json, ms: performance.now() - started };
  }

  async expect(status, method, path, opts) {
    const r = await this.call(method, path, opts);
    if (r.status !== status) {
      const code = r.json?.error?.code ?? r.json?.code ?? "";
      throw new Error(`${method} ${path} → ${r.status} ${code} (expected ${status})`);
    }
    return r;
  }
}

export const uuid = () => randomUUID();
export const sha256hex = (buf) => createHash("sha256").update(buf).digest("hex");
/** Messaging's binary encoding: base64url without padding. */
export const b64 = (bytes) => Buffer.from(bytes).toString("base64url");

/** A valid PNG of random pixels (incompressible), about `approxBytes` large. */
export function noisePng(approxBytes) {
  const width = 256;
  const height = Math.max(1, Math.ceil(approxBytes / (width * 3)));
  const row = 1 + width * 3;
  const raw = Buffer.alloc(row * height);
  const noise = randomBytes(width * 3 * height);
  for (let y = 0; y < height; y++) noise.copy(raw, y * row + 1, y * width * 3, (y + 1) * width * 3);
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(td) >>> 0);
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // RGB
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** Opens an authenticated realtime WebSocket (single-use ticket in the subprotocol). */
export function openRealtime(base, ticket, onFrame) {
  const u = new URL(base);
  u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
  u.pathname = `${u.pathname.replace(/\/$/, "")}/v1/realtime`;
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(u, ["cfmeet.messaging.v1", `cfmeet.ticket.${ticket}`]);
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error("realtime handshake timed out"));
    }, 15000);
    ws.onopen = () => {
      clearTimeout(timer);
      resolve(ws);
    };
    ws.onerror = () => {
      clearTimeout(timer);
      reject(new Error("realtime handshake refused"));
    };
    ws.onmessage = (m) => {
      try {
        onFrame(JSON.parse(typeof m.data === "string" ? m.data : Buffer.from(m.data).toString()));
      } catch {
        /* non-JSON frames are ignored */
      }
    };
  });
}
