// Webhook signature verification (the one CF Meet scheme, ADR 0028):
//
//   CF-Webhook-Id:        <delivery id>
//   CF-Webhook-Event-Id:  <event id>          (stable across retries / replays: dedupe on it)
//   CF-Webhook-Timestamp: <unix seconds>
//   CF-Webhook-Signature: v1=<hex HMAC-SHA256(secret, "v1:" + timestamp + ":" + delivery id + ":" + raw body)>
//
// The HMAC key is the full signing secret string (`whsec_...`) as UTF-8 bytes. Verify the
// RAW body bytes exactly as received (never a re-serialized JSON object), refuse
// timestamps outside the tolerance (default 300 s) and compare in constant time
// (Web Crypto's HMAC verify). Same order of checks as the reference verifier
// (packages/webhooks/src/signing.rs): timestamp format, tolerance, signature format, MAC.

import { CfMeetConfigurationError, CfMeetWebhookSignatureError } from './errors.js';

export const WEBHOOK_HEADERS = {
  deliveryId: 'cf-webhook-id',
  eventId: 'cf-webhook-event-id',
  timestamp: 'cf-webhook-timestamp',
  signature: 'cf-webhook-signature',
} as const;

export const DEFAULT_WEBHOOK_TOLERANCE_SECONDS = 300;

/** Request headers as a Headers object or a plain record (e.g. Node's IncomingHttpHeaders). */
export type HeaderSource = Headers | { readonly [name: string]: string | readonly string[] | undefined };

export interface VerifyWebhookInput {
  /** The endpoint's signing secret (`whsec_...`), from your server-side secret store. */
  readonly secret: string;
  /** The raw request body exactly as received. */
  readonly rawBody: string | Uint8Array | ArrayBuffer;
  /** The request headers (or pass the fields below individually). */
  readonly headers?: HeaderSource;
  readonly deliveryId?: string;
  readonly eventId?: string;
  readonly timestamp?: string;
  readonly signature?: string;
  /** Current Unix time in seconds (defaults to the clock). */
  readonly nowSeconds?: number;
  readonly toleranceSeconds?: number;
}

export type WebhookVerificationResult =
  | {
      readonly valid: true;
      readonly deliveryId: string;
      /** Dedupe on this (stable across retries and replays); null if the header was absent. */
      readonly eventId: string | null;
      readonly timestamp: number;
    }
  | {
      readonly valid: false;
      readonly reason: 'MISMATCH' | 'STALE_TIMESTAMP' | 'MALFORMED';
      readonly detail: string;
    };

function header(h: HeaderSource | undefined, name: string): string | undefined {
  if (!h) return undefined;
  if (typeof (h as Headers).get === 'function') return (h as Headers).get(name) ?? undefined;
  const rec = h as { readonly [name: string]: string | readonly string[] | undefined };
  for (const [k, v] of Object.entries(rec)) {
    if (k.toLowerCase() !== name) continue;
    if (Array.isArray(v)) return v.length === 1 ? v[0] : undefined; // a repeated header is malformed
    return v as string | undefined;
  }
  return undefined;
}

function bytes(body: string | Uint8Array | ArrayBuffer): Uint8Array {
  if (typeof body === 'string') return new TextEncoder().encode(body);
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  throw new CfMeetConfigurationError('rawBody must be a string, Uint8Array or ArrayBuffer (the raw request body)');
}

function subtle(): SubtleCrypto {
  const s = (globalThis as { crypto?: Crypto }).crypto?.subtle;
  if (!s) throw new CfMeetConfigurationError('Web Crypto (crypto.subtle) is required for webhook verification');
  return s;
}

const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;

/** Verifies one webhook delivery. Never throws for a bad delivery: inspect `valid`. */
export async function verifyWebhookSignature(input: VerifyWebhookInput): Promise<WebhookVerificationResult> {
  if (typeof input.secret !== 'string' || input.secret === '') {
    throw new CfMeetConfigurationError('the webhook signing secret is required');
  }
  const raw = bytes(input.rawBody);
  const tolerance = input.toleranceSeconds ?? DEFAULT_WEBHOOK_TOLERANCE_SECONDS;
  if (!Number.isInteger(tolerance) || tolerance < 0) throw new CfMeetConfigurationError('toleranceSeconds must be a non-negative integer');
  const deliveryId = input.deliveryId ?? header(input.headers, WEBHOOK_HEADERS.deliveryId);
  const timestampHeader = input.timestamp ?? header(input.headers, WEBHOOK_HEADERS.timestamp);
  const signatureHeader = input.signature ?? header(input.headers, WEBHOOK_HEADERS.signature);
  const eventId = input.eventId ?? header(input.headers, WEBHOOK_HEADERS.eventId) ?? null;
  if (deliveryId === undefined || timestampHeader === undefined || signatureHeader === undefined) {
    return { valid: false, reason: 'MALFORMED', detail: 'missing CF-Webhook-Id, CF-Webhook-Timestamp or CF-Webhook-Signature' };
  }

  // 1. Timestamp: a signed 64-bit integer (surrounding whitespace allowed).
  const ts = timestampHeader.trim();
  if (!/^[+-]?\d{1,19}$/.test(ts)) return { valid: false, reason: 'MALFORMED', detail: 'timestamp is not an integer' };
  const timestamp = BigInt(ts);
  if (timestamp < I64_MIN || timestamp > I64_MAX) return { valid: false, reason: 'MALFORMED', detail: 'timestamp out of range' };

  // 2. Tolerance window.
  const nowSeconds = input.nowSeconds ?? Math.floor(Date.now() / 1000);
  const now = BigInt(Math.trunc(nowSeconds));
  const skew = now - timestamp;
  if ((skew < 0n ? -skew : skew) > BigInt(tolerance)) {
    return { valid: false, reason: 'STALE_TIMESTAMP', detail: `timestamp outside the ${tolerance} s tolerance` };
  }

  // 3. Signature format: v1=<hex>.
  const sig = signatureHeader.trim();
  if (!sig.startsWith('v1=')) return { valid: false, reason: 'MALFORMED', detail: 'unsupported signature version' };
  const hex = sig.slice(3);
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) {
    return { valid: false, reason: 'MALFORMED', detail: 'signature is not hex' };
  }
  const given = new Uint8Array(hex.length / 2);
  for (let i = 0; i < given.length; i++) given[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);

  // 4. MAC over "v1:" + timestamp + ":" + delivery id + ":" + raw body (constant-time verify).
  const prefix = new TextEncoder().encode(`v1:${timestamp.toString()}:${deliveryId}:`);
  const input2 = new Uint8Array(prefix.length + raw.length);
  input2.set(prefix, 0);
  input2.set(raw, prefix.length);
  const key = await subtle().importKey('raw', new TextEncoder().encode(input.secret), { name: 'HMAC', hash: 'SHA-256' }, false, [
    'verify',
  ]);
  const ok = given.length === 32 && (await subtle().verify('HMAC', key, given, input2));
  if (!ok) return { valid: false, reason: 'MISMATCH', detail: 'signature does not match' };
  return { valid: true, deliveryId, eventId, timestamp: Number(timestamp) };
}

/** The normalized webhook event body (contract cfmeet.platform.webhooks WebhookEvent). */
export interface WebhookEvent<T extends string = string> {
  readonly id: string;
  readonly type: T;
  readonly apiVersion: 'v1';
  readonly occurredAt: string;
  readonly correlationId?: string;
  readonly data: { readonly [key: string]: string | number };
}

export interface VerifiedWebhookEvent extends WebhookEvent {
  /** The delivery (CF-Webhook-Id). */
  readonly deliveryId: string;
  /** The dedupe key (CF-Webhook-Event-Id), when sent. */
  readonly eventId: string | null;
}

/** Verifies the delivery, then parses the event; throws CfMeetWebhookSignatureError if it is not authentic. */
export async function constructEvent(input: VerifyWebhookInput): Promise<VerifiedWebhookEvent> {
  const result = await verifyWebhookSignature(input);
  if (!result.valid) throw new CfMeetWebhookSignatureError(result.reason, result.detail);
  const text = typeof input.rawBody === 'string' ? input.rawBody : new TextDecoder('utf-8', { fatal: true }).decode(bytes(input.rawBody));
  let event: unknown;
  try {
    event = JSON.parse(text);
  } catch {
    throw new CfMeetWebhookSignatureError('MALFORMED', 'the signed body is not JSON');
  }
  const e = event as Partial<WebhookEvent> | null;
  if (!e || typeof e !== 'object' || typeof e.id !== 'string' || typeof e.type !== 'string' || typeof e.data !== 'object') {
    throw new CfMeetWebhookSignatureError('MALFORMED', 'the signed body is not a webhook event');
  }
  return { ...(e as WebhookEvent), deliveryId: result.deliveryId, eventId: result.eventId };
}
