import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { after, before, beforeEach, describe, test } from 'node:test';

import {
  CfMeetAbortError,
  CfMeetApiError,
  CfMeetAuthenticationError,
  CfMeetClient,
  CfMeetConfigurationError,
  CfMeetConflictError,
  CfMeetNotFoundError,
  CfMeetPermissionError,
  CfMeetQuotaExceededError,
  CfMeetRateLimitedError,
  CfMeetServerError,
  CfMeetTimeoutError,
  CfMeetUpstreamUnavailableError,
  CfMeetValidationError,
  DOMAIN_ACCESSORS,
  OPERATIONS,
  SDK_VERSION,
  UnknownOutcomeError,
  buildPath,
  parseRetryAfter,
  type CfMeetClientConfig,
} from '../src/index.js';
import { errorEnvelope, inBrowserLikeRuntime, json, startServer, type TestServer } from './helpers.js';

const CRED = 'cfmk_sbx_0123456789abcdef_unit-test-not-a-real-secret';
const CALL_ID = '7f1c0a52-8d8e-4bde-9a51-6c1e0f2b3a4d';

let server: TestServer;

function client(extra: Partial<CfMeetClientConfig> = {}): CfMeetClient {
  return new CfMeetClient({
    baseUrl: server.url,
    auth: { type: 'platformCredential', credential: CRED },
    retry: { initialDelayMs: 1, maxDelayMs: 5 },
    ...extra,
  });
}

before(async () => {
  server = await startServer();
});
after(async () => {
  await server.close();
});
beforeEach(() => {
  server.requests.length = 0;
  server.handler = (_q, res) => json(res, 200, { ok: true }, { 'x-request-id': 'req_ok', 'x-correlation-id': 'cor_ok' });
});

describe('configuration', () => {
  test('SDK_VERSION matches package.json', () => {
    const pkg = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8')) as { version: string };
    assert.equal(SDK_VERSION, pkg.version);
  });

  test('baseUrl must be https except localhost', () => {
    assert.throws(() => new CfMeetClient({ baseUrl: 'http://api.example.com' }), CfMeetConfigurationError);
    assert.throws(() => new CfMeetClient({ baseUrl: 'not a url' }), CfMeetConfigurationError);
    assert.throws(() => new CfMeetClient({ baseUrl: 'https://u:p@api.example.com' }), CfMeetConfigurationError);
    assert.equal(new CfMeetClient({ baseUrl: 'https://api.example.com/' }).baseUrl, 'https://api.example.com');
    assert.equal(new CfMeetClient({ baseUrl: 'http://api.example.com', allowInsecureHttp: true }).baseUrl, 'http://api.example.com');
  });

  test('a platform credential is refused in a browser-like runtime by default', async () => {
    await inBrowserLikeRuntime(() => {
      assert.throws(() => client(), (e: unknown) => e instanceof CfMeetConfigurationError && /server-side only/.test(e.message));
      // a cfmk_ value disguised as another auth type is refused too
      assert.throws(() => client({ auth: { type: 'userSession', token: CRED } }), CfMeetConfigurationError);
    });
  });

  test('a provider returning a cfmk_ credential is refused in a browser-like runtime at request time', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: () => CRED } });
    await inBrowserLikeRuntime(async () => {
      await assert.rejects(c.developerPlatform.listOrganizations(), CfMeetConfigurationError);
    });
    assert.equal(server.requests.length, 0);
  });

  test('the escape hatch allows a platform credential in a trusted browser-like server runtime', async () => {
    await inBrowserLikeRuntime(async () => {
      const c = client({ dangerouslyAllowPlatformCredentialInBrowserLikeRuntime: true });
      await c.sms.getMessage({ path: { messageId: 'm1' } });
    });
    assert.equal(server.requests.length, 1);
  });

  test('user sessions work in a browser-like runtime', async () => {
    await inBrowserLikeRuntime(async () => {
      const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: 'eyJ.session.jwt' } });
      await c.developerPlatform.listOrganizations();
    });
    assert.equal(server.requests[0]!.headers.authorization, 'Bearer eyJ.session.jwt');
    assert.equal(server.requests[0]!.headers['user-agent'] === undefined || !String(server.requests[0]!.headers['user-agent']).includes('cfmeet-platform-sdk'), true);
  });

  test('every REST operation has a typed accessor; WebSocket operations do not', () => {
    const c = client();
    let n = 0;
    for (const op of OPERATIONS) {
      const domain = (c as unknown as Record<string, Record<string, unknown>>)[DOMAIN_ACCESSORS[op.domain as keyof typeof DOMAIN_ACCESSORS]]!;
      if (op.protocol === 'REST') {
        assert.equal(typeof domain[op.name], 'function', `${op.domain}.${op.name}`);
        assert.equal(typeof (domain[op.name] as { all?: unknown }).all, op.pagination ? 'function' : 'undefined');
        n++;
      } else {
        assert.equal(domain[op.name], undefined);
      }
    }
    assert.equal(n, OPERATIONS.filter((o) => o.protocol === 'REST').length);
  });

  test('the paid telecom operations are exactly the NEVER class', () => {
    const cfg = JSON.parse(readFileSync(new URL('../../../generation.json', import.meta.url), 'utf8')) as { paidTelecomOperations: string[] };
    const never = OPERATIONS.filter((o) => o.retry === 'NEVER').map((o) => `${o.method} ${o.path}`).sort();
    assert.deepEqual(never, [...cfg.paidTelecomOperations].sort());
    assert.ok(OPERATIONS.filter((o) => o.retry === 'NEVER').every((o) => o.paidTelecom));
  });
});

describe('authentication headers', () => {
  test('platform credential → Authorization: Bearer', async () => {
    const res = await client().gateway.getDeveloperPlatformContext();
    assert.equal(server.requests[0]!.headers.authorization, `Bearer ${CRED}`);
    assert.equal(server.requests[0]!.headers['x-participant-token'], undefined);
    assert.match(String(server.requests[0]!.headers['user-agent']), /^cfmeet-platform-sdk-typescript\//);
    assert.equal(res.requestId, 'req_ok');
    assert.equal(res.correlationId, 'cor_ok');
  });

  test('participant token → X-Participant-Token (no Authorization)', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'participantToken', token: 'pt_123' } });
    await c.meetings.v2RealtimeTicket({ path: { id: CALL_ID } });
    assert.equal(server.requests[0]!.headers['x-participant-token'], 'pt_123');
    assert.equal(server.requests[0]!.headers.authorization, undefined);
  });

  test('client token → Authorization: CF-Client-Token', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'clientToken', token: 'ct.abc.def' } });
    await c.messaging.issueRealtimeTicket();
    assert.equal(server.requests[0]!.headers.authorization, 'CF-Client-Token ct.abc.def');
  });

  test('provider functions are called per request', async () => {
    let n = 0;
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: () => `tok${++n}` } });
    await c.developerPlatform.listOrganizations();
    await c.developerPlatform.listOrganizations();
    assert.deepEqual(
      server.requests.map((r) => r.headers.authorization),
      ['Bearer tok1', 'Bearer tok2'],
    );
  });

  test('anonymous operations never carry the configured credential', async () => {
    await client().meetings.v2ResolveInvitation({ path: { token: 'inv-token' } });
    assert.equal(server.requests[0]!.headers.authorization, undefined);
    await client().discovery();
    assert.equal(server.requests[1]!.url, '/discovery');
    assert.equal(server.requests[1]!.headers.authorization, undefined);
  });

  test('an operation that does not accept the configured auth type is refused before sending', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: 'jwt' } });
    await assert.rejects(c.sms.sendMessage({ body: { from: { numberResourceId: CALL_ID }, to: '+15550100', text: 'hi' } }), CfMeetConfigurationError);
    const anon = new CfMeetClient({ baseUrl: server.url });
    await assert.rejects(anon.sms.getMessage({ path: { messageId: 'm' } }), CfMeetConfigurationError);
    assert.equal(server.requests.length, 0);
  });

  test('a per-call auth override is honoured', async () => {
    await client().messaging.issueRealtimeTicket(undefined, { auth: { type: 'clientToken', token: 'ct' } });
    assert.equal(server.requests[0]!.headers.authorization, 'CF-Client-Token ct');
  });

  test('no client request id is invented (the gateway generates its own)', async () => {
    await client().gateway.getDeveloperPlatformContext();
    const h = server.requests[0]!.headers;
    assert.equal(h['x-request-id'], undefined);
    assert.equal(h['x-correlation-id'], undefined);
  });
});

describe('request building', () => {
  test('path parameters are percent-encoded; / and .. are refused', async () => {
    await client().sms.getMessage({ path: { messageId: 'a b?c#d%' } });
    assert.equal(server.requests[0]!.url, '/v1/sms/messages/a%20b%3Fc%23d%25');
    for (const bad of ['../admin', 'a/b', '..', '.', '', 'a\\b', 'x\u0000y']) {
      await assert.rejects(client().sms.getMessage({ path: { messageId: bad } }), CfMeetConfigurationError, JSON.stringify(bad));
    }
    assert.equal(server.requests.length, 1);
    assert.throws(() => buildPath({ path: '/v1/x/{id}', pathParams: ['id'] }, {}), CfMeetConfigurationError);
    assert.throws(() => buildPath({ path: '/v1/x/{id}', pathParams: ['id'] }, { id: '1', other: '2' }), CfMeetConfigurationError);
  });

  test('query: documented parameters only, arrays repeat, null omitted', async () => {
    await client().sms.listMessages({ query: { direction: 'OUTBOUND', limit: 5, cursor: undefined } });
    assert.equal(server.requests[0]!.url, '/v1/sms/messages?direction=OUTBOUND&limit=5');
    await assert.rejects(
      client().request(CfMeetClient.operation('sms', 'listMessages'), { query: { secret: 'x' } }),
      CfMeetConfigurationError,
    );
  });

  test('headers: only documented headers; required ones enforced', async () => {
    const c = client();
    await c.messaging.getConversation({ path: { conversationId: CALL_ID }, headers: { 'cf-acting-principal': CALL_ID } });
    assert.equal(server.requests[0]!.headers['cf-acting-principal'], CALL_ID);
    const spec = CfMeetClient.operation('messaging', 'sendMessage');
    await assert.rejects(c.request(spec, { path: { conversationId: CALL_ID }, body: {} }), CfMeetConfigurationError);
    await assert.rejects(
      c.request(CfMeetClient.operation('sms', 'getMessage'), { path: { messageId: 'm' }, headers: { 'x-cf-tenant-id': 't' } }),
      CfMeetConfigurationError,
    );
    await assert.rejects(
      c.request(CfMeetClient.operation('sms', 'getMessage'), { path: { messageId: 'm' }, headers: { authorization: 'Bearer x' } }),
      CfMeetConfigurationError,
    );
    assert.equal(server.requests.length, 1);
  });

  test('gateway-forwarded conditional headers via options.headers', async () => {
    await client().sms.getMessage({ path: { messageId: 'm' } }, { headers: { 'if-none-match': '"v1"', 'accept-language': 'fr' } });
    assert.equal(server.requests[0]!.headers['if-none-match'], '"v1"');
    assert.equal(server.requests[0]!.headers['accept-language'], 'fr');
    await assert.rejects(
      client().sms.getMessage({ path: { messageId: 'm' } }, { headers: { 'x-forwarded-for': '1.2.3.4' } as never }),
      CfMeetConfigurationError,
    );
  });

  test('JSON body; body rules enforced', async () => {
    await client().calling.createBlock({ body: { blockedPrincipalId: CALL_ID } as never });
    assert.equal(server.requests[0]!.headers['content-type'], 'application/json');
    assert.deepEqual(JSON.parse(server.requests[0]!.body), { blockedPrincipalId: CALL_ID });
    await assert.rejects(client().request(CfMeetClient.operation('calling', 'createBlock'), {}), CfMeetConfigurationError);
    await assert.rejects(
      client().request(CfMeetClient.operation('sms', 'getMessage'), { path: { messageId: 'm' }, body: {} }),
      CfMeetConfigurationError,
    );
  });

  test('empty and binary responses', async () => {
    server.handler = (_q, res) => {
      res.writeHead(204, { 'x-request-id': 'r204' });
      res.end();
    };
    const empty = await client().sms.getMessage({ path: { messageId: 'm' } });
    assert.equal(empty.data, undefined);
    assert.equal(empty.requestId, 'r204');
    server.handler = (_q, res) => {
      res.writeHead(200, { 'content-type': 'application/octet-stream' });
      res.end(Buffer.from([1, 2, 3]));
    };
    const bin = await client().sms.getMessage({ path: { messageId: 'm' } });
    assert.ok(bin.data instanceof ArrayBuffer);
    assert.deepEqual([...new Uint8Array(bin.data as unknown as ArrayBuffer)], [1, 2, 3]);
  });

  test('WebSocket operations are refused by request()', async () => {
    await assert.rejects(client().request(CfMeetClient.operation('messaging', 'connectRealtime')), CfMeetConfigurationError);
  });
});

describe('error mapping', () => {
  const cases: [number, string, new (...a: never[]) => CfMeetApiError, string][] = [
    [400, 'VALIDATION_ERROR', CfMeetValidationError, 'validation'],
    [401, 'AUTHENTICATION_FAILED', CfMeetAuthenticationError, 'authentication'],
    [403, 'INSUFFICIENT_SCOPE', CfMeetPermissionError, 'permission'],
    [404, 'NOT_FOUND', CfMeetNotFoundError, 'not_found'],
    [409, 'STALE_CALL_VERSION', CfMeetConflictError, 'conflict'],
    [429, 'RATE_LIMITED', CfMeetRateLimitedError, 'rate_limited'],
    [429, 'QUOTA_EXCEEDED', CfMeetQuotaExceededError, 'quota_exceeded'],
    [502, 'UPSTREAM_UNAVAILABLE', CfMeetUpstreamUnavailableError, 'upstream_unavailable'],
    [503, 'SERVICE_UNAVAILABLE', CfMeetUpstreamUnavailableError, 'upstream_unavailable'],
    [500, 'INTERNAL_ERROR', CfMeetServerError, 'server'],
  ];
  for (const [status, code, cls, kind] of cases) {
    test(`${status} ${code} → ${cls.name}`, async () => {
      server.handler = (_q, res) =>
        errorEnvelope(res, status, code, {
          'x-correlation-id': 'cor_1',
          'retry-after': '3600',
          'x-cf-quota-window': 'requests-per-day',
          'x-cf-quota-level': 'environment',
        });
      const err = await client({ maxRetries: 0 }).sms.getMessage({ path: { messageId: 'm' } }).then(
        () => assert.fail('expected an error'),
        (e: unknown) => e,
      );
      assert.ok(err instanceof cls, `${(err as Error).name}`);
      assert.ok(err instanceof CfMeetApiError);
      const e = err as CfMeetApiError;
      assert.equal(e.kind, kind);
      assert.equal(e.status, status);
      assert.equal(e.code, code);
      assert.equal(e.requestId, 'req_test_1');
      assert.equal(e.correlationId, 'cor_1');
      assert.equal(e.retryAfterMs, 3_600_000);
      assert.equal(e.quota?.window, 'requests-per-day');
      assert.equal(e.quota?.level, 'environment');
      assert.deepEqual(e.operation, { domain: 'sms', name: 'getMessage', method: 'GET', path: '/v1/sms/messages/{messageId}' });
    });
  }

  test('a non-envelope error body still yields a typed error', async () => {
    server.handler = (_q, res) => {
      res.writeHead(404, { 'content-type': 'text/plain' });
      res.end('nope');
    };
    const e = (await client().sms.getMessage({ path: { messageId: 'm' } }).catch((x: unknown) => x)) as CfMeetApiError;
    assert.ok(e instanceof CfMeetNotFoundError);
    assert.equal(e.code, 'HTTP_404');
    assert.equal(e.requestId, null);
  });

  test('quota headers are surfaced on success', async () => {
    server.handler = (_q, res) =>
      json(res, 200, { ok: true }, {
        'x-cf-quota-limit-minute': '600',
        'x-cf-quota-remaining-minute': '599',
        'x-cf-quota-reset': '42',
        'x-cf-quota-limit-day': '100000',
      });
    const r = await client().gateway.getDeveloperPlatformContext();
    assert.deepEqual(r.quota, {
      limitPerMinute: 600,
      remainingThisMinute: 599,
      resetSeconds: 42,
      limitPerDay: 100000,
      window: null,
      level: null,
    });
  });
});

describe('Retry-After', () => {
  test('parses seconds and HTTP dates', () => {
    const now = Date.parse('Wed, 21 Oct 2026 07:28:00 GMT');
    assert.equal(parseRetryAfter('3', now), 3000);
    assert.equal(parseRetryAfter(' 0 ', now), 0);
    assert.equal(parseRetryAfter('Wed, 21 Oct 2026 07:28:05 GMT', now), 5000);
    assert.equal(parseRetryAfter('Wed, 21 Oct 2026 07:27:00 GMT', now), 0);
    assert.equal(parseRetryAfter('soon', now), null);
    assert.equal(parseRetryAfter(null, now), null);
  });

  test('a 429 is retried after Retry-After seconds', async () => {
    let n = 0;
    server.handler = (_q, res) => (++n === 1 ? errorEnvelope(res, 429, 'RATE_LIMITED', { 'retry-after': '1' }) : json(res, 200, { ok: 1 }));
    const t0 = Date.now();
    const r = await client().sms.getMessage({ path: { messageId: 'm' } });
    assert.ok(Date.now() - t0 >= 900, 'waited for Retry-After');
    assert.equal(r.attempts, 2);
  });

  test('a 429 with an HTTP-date Retry-After is honoured', async () => {
    let n = 0;
    server.handler = (_q, res) =>
      ++n === 1
        ? errorEnvelope(res, 429, 'RATE_LIMITED', { 'retry-after': new Date(Date.now() - 1000).toUTCString() })
        : json(res, 200, { ok: 1 });
    const r = await client().sms.getMessage({ path: { messageId: 'm' } });
    assert.equal(r.attempts, 2);
  });

  test('a Retry-After beyond the cap is not waited for', async () => {
    server.handler = (_q, res) => errorEnvelope(res, 429, 'QUOTA_EXCEEDED', { 'retry-after': '86400' });
    const t0 = Date.now();
    const e = (await client().sms.getMessage({ path: { messageId: 'm' } }).catch((x: unknown) => x)) as CfMeetQuotaExceededError;
    assert.ok(e instanceof CfMeetQuotaExceededError);
    assert.equal(e.retryAfterMs, 86_400_000);
    assert.ok(Date.now() - t0 < 1000);
    assert.equal(server.requests.length, 1);
  });
});

describe('retry classes', () => {
  test('SAFE: network-ish statuses are retried up to maxRetries', async () => {
    let n = 0;
    server.handler = (_q, res) => (++n < 3 ? errorEnvelope(res, [502, 503, 504][n - 1]!, 'UPSTREAM_UNAVAILABLE') : json(res, 200, { ok: 1 }));
    const r = await client().sms.getMessage({ path: { messageId: 'm' } });
    assert.equal(r.attempts, 3);
    assert.equal(server.requests.length, 3);
  });

  test('SAFE: gives up after maxRetries and throws the last error', async () => {
    server.handler = (_q, res) => errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE');
    await assert.rejects(client({ maxRetries: 2 }).sms.getMessage({ path: { messageId: 'm' } }), CfMeetUpstreamUnavailableError);
    assert.equal(server.requests.length, 3);
  });

  test('SAFE: a 4xx is not retried', async () => {
    server.handler = (_q, res) => errorEnvelope(res, 404, 'NOT_FOUND');
    await assert.rejects(client().sms.getMessage({ path: { messageId: 'm' } }), CfMeetNotFoundError);
    assert.equal(server.requests.length, 1);
  });

  test('SAFE: a connection reset is retried', async () => {
    let n = 0;
    server.handler = (_q, res) => {
      if (++n === 1) res.socket?.destroy();
      else json(res, 200, { ok: 1 });
    };
    const r = await client().sms.getMessage({ path: { messageId: 'm' } });
    assert.equal(r.attempts, 2);
  });

  test('IDEMPOTENT: retried with the SAME Idempotency-Key (generated)', async () => {
    let n = 0;
    server.handler = (_q, res) => (++n < 3 ? errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE') : json(res, 200, { ok: 1 }));
    const r = await client().calling.endCall({ path: { call_id: CALL_ID }, body: { expectedVersion: 3 } });
    const keys = server.requests.map((q) => q.headers['idempotency-key']);
    assert.equal(keys.length, 3);
    assert.match(String(keys[0]), /^[0-9a-f-]{36}$/);
    assert.ok(keys.every((k) => k === keys[0]));
    assert.equal(r.idempotencyKey, keys[0]);
  });

  test('IDEMPOTENT: a caller key is used as is', async () => {
    let n = 0;
    server.handler = (_q, res) => (++n < 2 ? errorEnvelope(res, 502, 'UPSTREAM_UNAVAILABLE') : json(res, 200, { ok: 1 }));
    await client().calling.endCall({ path: { call_id: CALL_ID }, body: { expectedVersion: 3 }, idempotencyKey: 'my-key-1' });
    assert.deepEqual(server.requests.map((q) => q.headers['idempotency-key']), ['my-key-1', 'my-key-1']);
  });

  test('IDEMPOTENT (PUT/DELETE without a key): retried; no key is sent', async () => {
    let n = 0;
    server.handler = (_q, res) => (++n < 2 ? errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE') : json(res, 200, { ok: 1 }));
    await client().calling.deleteWebhook({ path: { webhook_id: 'w1' }, query: { expectedVersion: 1 } });
    assert.equal(server.requests.length, 2);
    assert.equal(server.requests[0]!.headers['idempotency-key'], undefined);
  });

  test('an idempotency key on an operation that takes none is refused', async () => {
    await assert.rejects(
      client().request(CfMeetClient.operation('calling', 'createBlock'), { body: {}, idempotencyKey: 'k' }),
      CfMeetConfigurationError,
    );
  });

  test('NO_RETRY: a 503 is not retried', async () => {
    server.handler = (_q, res) => errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE');
    await assert.rejects(client().calling.createBlock({ body: {} as never }), CfMeetUpstreamUnavailableError);
    assert.equal(server.requests.length, 1);
  });

  test('NO_RETRY: a timeout is not retried', async () => {
    server.handler = () => {};
    await assert.rejects(client({ timeoutMs: 100 }).calling.createBlock({ body: {} as never }), CfMeetTimeoutError);
    assert.equal(server.requests.length, 1);
  });

  test('NO_RETRY: a 429 is retried (nothing was done)', async () => {
    let n = 0;
    server.handler = (_q, res) => (++n < 2 ? errorEnvelope(res, 429, 'RATE_LIMITED') : json(res, 201, { ok: 1 }));
    const r = await client().calling.createBlock({ body: {} as never });
    assert.equal(r.status, 201);
    assert.equal(server.requests.length, 2);
  });

  const sms = { from: { numberResourceId: CALL_ID }, to: '+15550100', text: 'hello' };

  for (const status of [500, 502, 504]) {
    test(`NEVER: exactly one request; ${status} → UnknownOutcomeError with the idempotency key`, async () => {
      server.handler = (_q, res) => errorEnvelope(res, status, 'X', {}, 'req_never');
      const e = (await client({ maxRetries: 5 }).sms.sendMessage({ body: sms }).catch((x: unknown) => x)) as UnknownOutcomeError;
      assert.ok(e instanceof UnknownOutcomeError, String(e));
      assert.equal(server.requests.length, 1);
      const sent = server.requests[0]!.headers['idempotency-key'];
      assert.match(String(sent), /^[0-9a-f-]{36}$/);
      assert.equal(e.idempotencyKey, sent);
      assert.equal(e.status, status);
      assert.equal(e.requestId, 'req_never');
      assert.ok(e.cause instanceof CfMeetApiError);
      assert.equal(e.reason, { 500: 'server_error', 502: 'bad_gateway', 504: 'gateway_timeout' }[status]);
    });
  }

  test('NEVER: exactly one request; a timeout → UnknownOutcomeError', async () => {
    server.handler = () => {};
    const e = (await client({ timeoutMs: 100, maxRetries: 5 }).calling.createCall({
      body: { kind: 'VOICE', initiatorPrincipalId: CALL_ID, initiatorEndpointId: CALL_ID, targetPrincipalIds: [CALL_ID] } as never,
      idempotencyKey: 'call-attempt-1',
    }).catch((x: unknown) => x)) as UnknownOutcomeError;
    assert.ok(e instanceof UnknownOutcomeError);
    assert.equal(e.reason, 'timeout');
    assert.equal(e.idempotencyKey, 'call-attempt-1');
    assert.ok(e.cause instanceof CfMeetTimeoutError);
    assert.equal(server.requests.length, 1);
    assert.equal(e.operation.path, '/v1/calls');
  });

  test('NEVER: a connection lost after sending → UnknownOutcomeError, one request', async () => {
    server.handler = (_q, res) => void res.socket?.destroy();
    const e = await client({ maxRetries: 5 }).sms.sendMessage({ body: sms }).catch((x: unknown) => x);
    assert.ok(e instanceof UnknownOutcomeError);
    assert.equal((e as UnknownOutcomeError).reason, 'network');
    assert.equal(server.requests.length, 1);
  });

  test('NEVER: a 429 or 503 is not retried either (typed error, not unknown)', async () => {
    server.handler = (_q, res) => errorEnvelope(res, 429, 'RATE_LIMITED', { 'retry-after': '0' });
    await assert.rejects(client().sms.sendMessage({ body: sms }), CfMeetRateLimitedError);
    server.handler = (_q, res) => errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE');
    await assert.rejects(client().sms.sendMessage({ body: sms }), CfMeetUpstreamUnavailableError);
    assert.equal(server.requests.length, 2);
  });

  test('NEVER: a refused connection (nothing sent) is a network error, not unknown', async () => {
    const closed = await startServer();
    const url = closed.url;
    await closed.close();
    const c = new CfMeetClient({ baseUrl: url, auth: { type: 'platformCredential', credential: CRED } });
    const e = await c.sms.sendMessage({ body: sms }).catch((x: unknown) => x);
    assert.equal((e as Error).name, 'CfMeetNetworkError');
  });

  test('idempotent replay flag is exposed', async () => {
    server.handler = (_q, res) => json(res, 202, { id: 'm1' }, { 'idempotent-replayed': 'true' });
    const r = await client().sms.sendMessage({ body: sms, idempotencyKey: 'sms-1' });
    assert.equal(r.idempotentReplayed, true);
    assert.equal(r.idempotencyKey, 'sms-1');
    assert.equal(server.requests[0]!.headers['idempotency-key'], 'sms-1');
    server.handler = (_q, res) => json(res, 202, { id: 'm1' });
    assert.equal((await client().sms.sendMessage({ body: sms })).idempotentReplayed, false);
  });
});

describe('timeouts and cancellation', () => {
  test('per-request timeout', async () => {
    server.handler = () => {};
    const t0 = Date.now();
    await assert.rejects(client({ maxRetries: 0 }).sms.getMessage({ path: { messageId: 'm' } }, { timeoutMs: 80 }), CfMeetTimeoutError);
    assert.ok(Date.now() - t0 < 2000);
  });

  test('SAFE timeouts are retried', async () => {
    let n = 0;
    server.handler = (_q, res) => {
      if (++n > 1) json(res, 200, { ok: 1 });
    };
    const r = await client({ timeoutMs: 80 }).sms.getMessage({ path: { messageId: 'm' } });
    assert.equal(r.attempts, 2);
  });

  test('caller AbortSignal cancels the call and pending retries', async () => {
    server.handler = () => {};
    const ac = new AbortController();
    setTimeout(() => ac.abort(), 50);
    await assert.rejects(client().sms.getMessage({ path: { messageId: 'm' } }, { signal: ac.signal }), CfMeetAbortError);
    const pre = new AbortController();
    pre.abort();
    await assert.rejects(client().sms.getMessage({ path: { messageId: 'm' } }, { signal: pre.signal }), CfMeetAbortError);
    // aborting during a retry backoff
    server.handler = (_q, res) => errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE');
    const ac2 = new AbortController();
    const c = new CfMeetClient({
      baseUrl: server.url,
      auth: { type: 'platformCredential', credential: CRED },
      retry: { initialDelayMs: 5000, maxDelayMs: 5000 },
    });
    setTimeout(() => ac2.abort(), 100);
    const t0 = Date.now();
    await assert.rejects(c.sms.getMessage({ path: { messageId: 'm' } }, { signal: ac2.signal }), CfMeetAbortError);
    assert.ok(Date.now() - t0 < 2000);
  });

  test('aborting a NEVER operation in flight is an unknown outcome', async () => {
    server.handler = () => {};
    const ac = new AbortController();
    setTimeout(() => ac.abort(), 50);
    const e = await client()
      .sms.sendMessage({ body: { from: { numberResourceId: CALL_ID }, to: '+15550100', text: 'x' } }, { signal: ac.signal })
      .catch((x: unknown) => x);
    assert.ok(e instanceof UnknownOutcomeError);
    assert.equal((e as UnknownOutcomeError).reason, 'aborted');
  });
});

describe('pagination', () => {
  test('all() follows nextCursor and yields every item; pages() yields pages', async () => {
    const pages: Record<string, { messages: { id: string }[]; nextCursor: string | null }> = {
      '': { messages: [{ id: 'a' }, { id: 'b' }], nextCursor: 'c2' },
      c2: { messages: [{ id: 'c' }], nextCursor: 'c3' },
      c3: { messages: [], nextCursor: null },
    };
    server.handler = (q, res) => {
      const cursor = new URL(q.url, 'http://x').searchParams.get('cursor') ?? '';
      json(res, 200, pages[cursor]);
    };
    const ids: string[] = [];
    for await (const m of client().sms.listMessages.all({ query: { limit: 2 } })) ids.push((m as unknown as { id: string }).id);
    assert.deepEqual(ids, ['a', 'b', 'c']);
    assert.deepEqual(
      server.requests.map((r) => r.url),
      ['/v1/sms/messages?cursor=&limit=2', '/v1/sms/messages?cursor=c2&limit=2', '/v1/sms/messages?cursor=c3&limit=2'].map((u) =>
        u.replace('cursor=&', ''),
      ),
    );
    let count = 0;
    for await (const p of client().sms.listMessages.pages()) {
      assert.equal(p.status, 200);
      count++;
    }
    assert.equal(count, 3);
  });

  test('a repeated cursor stops with an error instead of looping', async () => {
    server.handler = (_q, res) => json(res, 200, { messages: [{ id: 'x' }], nextCursor: 'same' });
    await assert.rejects(async () => {
      for await (const _ of client().sms.listMessages.all()) void _;
    }, /repeated a cursor/);
  });
});
