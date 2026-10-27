import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { describe, test } from 'node:test';

import { CfMeetWebhookSignatureError, constructEvent, verifyWebhookSignature } from '../src/index.js';

interface Vector {
  name: string;
  secret: string;
  signature: string;
  timestamp: string;
  deliveryId: string;
  body: string;
  now: number;
  expected: 'VALID' | 'MISMATCH' | 'STALE_TIMESTAMP' | 'MALFORMED';
}

const file = JSON.parse(
  readFileSync(new URL('../../../testdata/webhook-signature-vectors.json', import.meta.url), 'utf8'),
) as { toleranceSeconds: number; vectors: Vector[] };

describe('shared webhook signature vectors (Rust reference signer)', () => {
  assert.ok(file.vectors.length >= 12);
  for (const v of file.vectors) {
    test(`${v.name} → ${v.expected}`, async () => {
      // As separate fields...
      const r1 = await verifyWebhookSignature({
        secret: v.secret,
        rawBody: v.body,
        deliveryId: v.deliveryId,
        timestamp: v.timestamp,
        signature: v.signature,
        nowSeconds: v.now,
        toleranceSeconds: file.toleranceSeconds,
      });
      assert.equal(r1.valid ? 'VALID' : r1.reason, v.expected);
      // ...from Node-style headers with the raw body as bytes...
      const r2 = await verifyWebhookSignature({
        secret: v.secret,
        rawBody: Buffer.from(v.body, 'utf8'),
        headers: {
          'cf-webhook-id': v.deliveryId,
          'cf-webhook-event-id': 'whe_1',
          'cf-webhook-timestamp': v.timestamp,
          'cf-webhook-signature': v.signature,
        },
        nowSeconds: v.now,
      });
      assert.equal(r2.valid ? 'VALID' : r2.reason, v.expected);
      // ...and from a fetch Headers object (case-insensitive names) with an ArrayBuffer body.
      const bytes = new TextEncoder().encode(v.body);
      const r3 = await verifyWebhookSignature({
        secret: v.secret,
        rawBody: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
        headers: new Headers({ 'CF-Webhook-Id': v.deliveryId, 'CF-Webhook-Timestamp': v.timestamp, 'CF-Webhook-Signature': v.signature }),
        nowSeconds: v.now,
      });
      assert.equal(r3.valid ? 'VALID' : r3.reason, v.expected);
      if (r2.valid) {
        assert.equal(r2.deliveryId, v.deliveryId);
        assert.equal(r2.eventId, 'whe_1');
        assert.equal(r2.timestamp, Number(v.timestamp));
      }
    });
  }
});

describe('webhook verification details', () => {
  const secret = 'whsec_unit-test-only';
  const sign = (ts: string, id: string, body: string) =>
    `v1=${createHmac('sha256', secret).update(`v1:${ts}:${id}:${body}`).digest('hex')}`;

  test('missing headers are MALFORMED', async () => {
    const r = await verifyWebhookSignature({ secret, rawBody: '{}', headers: {} });
    assert.equal(r.valid, false);
    assert.equal(!r.valid && r.reason, 'MALFORMED');
  });

  test('upper-case hex and surrounding whitespace are accepted (as by the reference verifier)', async () => {
    const now = 1_760_000_000;
    const sig = sign(String(now), 'whd_x', '{"a":1}');
    const r = await verifyWebhookSignature({
      secret,
      rawBody: '{"a":1}',
      headers: { 'cf-webhook-id': 'whd_x', 'cf-webhook-timestamp': ` ${now} `, 'cf-webhook-signature': ` v1=${sig.slice(3).toUpperCase()} ` },
      nowSeconds: now,
    });
    assert.equal(r.valid, true);
  });

  test('a truncated signature is a MISMATCH, an odd-length one MALFORMED', async () => {
    const now = 1_760_000_000;
    const sig = sign(String(now), 'whd_x', '{}');
    const base = { secret, rawBody: '{}', deliveryId: 'whd_x', timestamp: String(now), nowSeconds: now };
    const short = await verifyWebhookSignature({ ...base, signature: sig.slice(0, 3 + 62) });
    assert.equal(!short.valid && short.reason, 'MISMATCH');
    const odd = await verifyWebhookSignature({ ...base, signature: sig.slice(0, 3 + 63) });
    assert.equal(!odd.valid && odd.reason, 'MALFORMED');
  });

  test('constructEvent verifies, then parses', async () => {
    const now = 1_760_000_000;
    const body = JSON.stringify({ id: 'whe_0123', type: 'sms.delivered', apiVersion: 'v1', occurredAt: '2026-10-23T00:00:00Z', data: { messageId: 'x' } });
    const headers = {
      'cf-webhook-id': 'whd_1',
      'cf-webhook-event-id': 'whe_0123',
      'cf-webhook-timestamp': String(now),
      'cf-webhook-signature': sign(String(now), 'whd_1', body),
    };
    const event = await constructEvent({ secret, rawBody: body, headers, nowSeconds: now });
    assert.equal(event.type, 'sms.delivered');
    assert.equal(event.deliveryId, 'whd_1');
    assert.equal(event.eventId, 'whe_0123');
    await assert.rejects(
      constructEvent({ secret, rawBody: body.replace('delivered', 'failed'), headers, nowSeconds: now }),
      (e: unknown) => e instanceof CfMeetWebhookSignatureError && e.reason === 'MISMATCH',
    );
    await assert.rejects(
      constructEvent({ secret, rawBody: body, headers, nowSeconds: now + 301 }),
      (e: unknown) => e instanceof CfMeetWebhookSignatureError && e.reason === 'STALE_TIMESTAMP',
    );
  });
});
