// The deterministic test fakes of `@cfmeet/platform-sdk/testing` (Part VI §52.3), used
// the way an application test would: a real CfMeetClient and realtime helpers wired to
// the fakes — no network, no real time passing.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { afterEach, beforeEach, describe, test } from 'node:test';

import {
  CfMeetAbortError,
  CfMeetApiError,
  CfMeetAuthenticationError,
  CfMeetClient,
  CfMeetConfigurationError,
  CfMeetConflictError,
  CfMeetNetworkError,
  CfMeetNotFoundError,
  CfMeetPermissionError,
  CfMeetQuotaExceededError,
  CfMeetRateLimitedError,
  CfMeetTimeoutError,
  CfMeetUpstreamUnavailableError,
  CfMeetValidationError,
  UnknownOutcomeError,
} from '../src/index.js';
import {
  FakeResponse,
  createFakeClock,
  createFakeFetch,
  createFakeWebSocket,
  fakeErrorCodeFor,
  fakeErrorEnvelope,
  fakePageBody,
  fakeQuotaHeaders,
  type FakeClock,
  type FakeFetch,
  type FakeWebSocketServer,
} from '../src/testing.js';

const CRED = 'cfmk_sbx_0123456789abcdef_unit-test-not-a-real-secret';
const NUMBER = '6f1d2c3b-4a59-4e8d-9c7b-1a2b3c4d5e6f';
const BASE = 'https://api.cfmeet.test';

let fake: FakeFetch;
let clock: FakeClock;
let client: CfMeetClient;
let started: number;

function makeClient(f: FakeFetch, c: FakeClock): CfMeetClient {
  return new CfMeetClient({
    baseUrl: BASE,
    auth: { type: 'platformCredential', credential: CRED },
    fetch: f.fetch,
    maxRetries: 3,
    retry: { sleep: c.sleep },
  });
}

beforeEach(() => {
  fake = createFakeFetch();
  clock = createFakeClock();
  client = makeClient(fake, clock);
  started = performance.now();
});
// Every test runs without sleeping: the SDK's waits go to the fake clock.
afterEach(() => assert.ok(performance.now() - started < 5_000, 'a test slept'));

async function rejection(p: Promise<unknown>): Promise<unknown> {
  try {
    await p;
  } catch (e) {
    return e;
  }
  assert.fail('expected a rejection');
}

const sendSms = (idempotencyKey = 'order-42') =>
  client.sms.sendMessage({ body: { from: { numberResourceId: NUMBER }, to: '+15550100', text: 'hi' }, idempotencyKey });

describe('package', () => {
  test('`./testing` is a separate, side-effect-free subpath export with no runtime dependencies', () => {
    const pkg = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'));
    assert.deepEqual(pkg.exports['./testing'], {
      types: './dist/testing.d.ts',
      import: './dist/testing.js',
      default: './dist/testing.js',
    });
    assert.equal(pkg.sideEffects, false);
    assert.equal(pkg.dependencies, undefined);
    const index = readFileSync(new URL('../../src/index.ts', import.meta.url), 'utf8');
    assert.doesNotMatch(index, /testing/);
  });
});

describe('createFakeFetch', () => {
  test('serves a typed call and records the request', async () => {
    fake.on(
      'sms',
      'getMessage',
      FakeResponse.json({ id: 'm1', status: 'DELIVERED' }, { correlationId: 'cor_1', quota: { limitPerMinute: 600, remainingThisMinute: 599 } }),
    );
    const r = await client.sms.getMessage({ path: { messageId: 'm 1' } });
    assert.equal(r.status, 200);
    assert.deepEqual(r.data, { id: 'm1', status: 'DELIVERED' });
    assert.equal(r.requestId, 'req_fake_000001');
    assert.equal(r.correlationId, 'cor_1');
    assert.equal(r.quota?.limitPerMinute, 600);
    assert.equal(r.quota?.remainingThisMinute, 599);
    assert.equal(r.attempts, 1);

    const [req] = fake.requests;
    assert.ok(req);
    assert.equal(fake.requests.length, 1);
    assert.equal(req.index, 1);
    assert.equal(req.requestId, 'req_fake_000001');
    assert.equal(req.method, 'GET');
    assert.equal(req.path, '/v1/sms/messages/m%201');
    assert.deepEqual(req.pathParams, { messageId: 'm 1' });
    assert.equal(req.operationName, 'sms.getMessage');
    assert.equal(req.headers.authorization, `Bearer ${CRED}`);
    assert.equal(req.idempotencyKey, null);
    assert.equal(req.body, undefined);
    assert.deepEqual(fake.requestsFor('sms', 'getMessage'), [req]);
  });

  test('records query, documented headers and the decoded JSON body', async () => {
    fake.on('messaging', 'listMessages', FakeResponse.json({ messages: [], nextAfterSequence: null }));
    fake.on('calling', 'holdCall', FakeResponse.json({ id: 'c1' }));
    await client.messaging.listMessages({
      path: { conversationId: 'conv_1' },
      query: { limit: 10, afterSequence: 4 },
      headers: { 'cf-acting-principal': 'usr_1' },
    });
    await client.calling.holdCall({ path: { call_id: 'c1' }, body: { expectedVersion: 3 }, idempotencyKey: 'hold-c1-v3' });
    const [list, hold] = fake.requests;
    assert.deepEqual(list?.query, { limit: '10', afterSequence: '4' });
    assert.equal(list?.headers['cf-acting-principal'], 'usr_1');
    assert.equal(hold?.method, 'POST');
    assert.deepEqual(hold?.body, { expectedVersion: 3 });
    assert.equal(hold?.idempotencyKey, 'hold-c1-v3');
    assert.equal(hold?.requestId, 'req_fake_000002');
  });

  test('operation names are validated against the catalogue', () => {
    const misnamed = 'sendMesage' as 'sendMessage'; // a JS caller (TypeScript refuses it at compile time)
    assert.throws(() => fake.on('sms', misnamed), (e: unknown) => e instanceof CfMeetConfigurationError && /sendMessage/.test(e.message));
    assert.throws(() => fake.on('telecom' as 'sms', 'getMessage'), CfMeetConfigurationError);
    assert.throws(() => FakeResponse.page('sms', 'getMessage', []), CfMeetConfigurationError);
    assert.throws(() => fake.onRequest('FETCH', '/x'), CfMeetConfigurationError);
  });

  test('an unregistered call gets the default 404 NOT_FOUND envelope', async () => {
    const e = await rejection(client.sms.getMessage({ path: { messageId: 'm1' } }));
    assert.ok(e instanceof CfMeetNotFoundError);
    assert.equal(e.code, 'NOT_FOUND');
    assert.equal(e.requestId, 'req_fake_000001');
    assert.equal(fake.unmatched[0]?.operationName, 'sms.getMessage');
  });

  test('method + path routes (templates too) take precedence', async () => {
    fake.on('sms', 'getMessage', FakeResponse.json({ from: 'operation' }));
    fake.onRequest('GET', '/v1/sms/messages/{messageId}', FakeResponse.json({ from: 'template' }));
    fake.onRequest('get', '/v1/sms/messages/special', FakeResponse.json({ from: 'literal' }));
    const from = async (id: string) => ((await client.sms.getMessage({ path: { messageId: id } })).data as unknown as { from: string }).from;
    assert.equal(await from('special'), 'literal');
    assert.equal(await from('other'), 'template');

    const discovery = fake.onRequest('GET', '/discovery', FakeResponse.json({ apis: [] }));
    await client.discovery();
    assert.equal(discovery.callCount, 1);
    assert.equal(fake.requests.at(-1)?.headers.authorization, undefined);
  });

  test('scripted sequences: replies in order, the last one repeats', async () => {
    let n = 0;
    const route = fake
      .on('sms', 'getMessage', FakeResponse.json({ n: 1 }))
      .thenReplyWith((req) => FakeResponse.json({ n: 2, id: req.pathParams.messageId, seen: ++n }));
    const get = async () => (await client.sms.getMessage({ path: { messageId: 'x' } })).data;
    assert.deepEqual(await get(), { n: 1 });
    assert.equal(route.exhausted, false);
    assert.deepEqual(await get(), { n: 2, id: 'x', seen: 1 });
    assert.deepEqual(await get(), { n: 2, id: 'x', seen: 2 });
    assert.equal(route.exhausted, true);
    assert.equal(route.callCount, 3);
  });

  test('custom request ids, path prefixes and reset', async () => {
    const f = createFakeFetch({ pathPrefix: '/gw', requestId: (n) => `req_${String(n).padStart(32, '0')}` });
    const c = new CfMeetClient({ baseUrl: `${BASE}/gw`, auth: { type: 'platformCredential', credential: CRED }, fetch: f.fetch });
    f.on('sms', 'getMessage', FakeResponse.json({}));
    const r = await c.sms.getMessage({ path: { messageId: 'm1' } });
    assert.equal(r.requestId, `req_${'1'.padStart(32, '0')}`);
    assert.equal(f.requests[0]?.path, '/v1/sms/messages/m1');
    f.reset();
    assert.equal(f.requests.length, 0);
    assert.ok((await rejection(c.sms.getMessage({ path: { messageId: 'm1' } }))) instanceof CfMeetNotFoundError);
  });
});

describe('retries by retry class', () => {
  test('SAFE: 503, network error and timeout are retried', async () => {
    fake
      .on('sms', 'getMessage', FakeResponse.error(503))
      .thenReply(FakeResponse.networkError(), FakeResponse.timeout(), FakeResponse.json({ id: 'm1' }));
    const r = await client.sms.getMessage({ path: { messageId: 'm1' } });
    assert.equal(r.attempts, 4);
    assert.equal(r.requestId, 'req_fake_000004');
    assert.equal(fake.requests.length, 4);
    assert.equal(clock.sleeps.length, 3);
  });

  test('SAFE: Retry-After is honoured on the fake clock, not slept', async () => {
    fake
      .on('sms', 'getMessage', FakeResponse.error(429, { retryAfter: 20, quota: { remainingThisMinute: 0 } }))
      .thenReply(FakeResponse.json({ id: 'm1' }));
    const r = await client.sms.getMessage({ path: { messageId: 'm1' } });
    assert.equal(r.attempts, 2);
    assert.deepEqual(clock.sleeps, [20_000]);
    assert.equal(clock.elapsedMs, 20_000);
  });

  test('SAFE: retries are bounded by maxRetries', async () => {
    fake.on('sms', 'getMessage', FakeResponse.error(503));
    const e = await rejection(client.sms.getMessage({ path: { messageId: 'm1' } }));
    assert.ok(e instanceof CfMeetUpstreamUnavailableError);
    assert.equal(e.requestId, 'req_fake_000004');
    assert.equal(fake.requests.length, 4); // 1 + maxRetries (3)
  });

  test('a timeout that is not retried surfaces as CfMeetTimeoutError', async () => {
    const c = new CfMeetClient({ baseUrl: BASE, auth: { type: 'platformCredential', credential: CRED }, fetch: fake.fetch, maxRetries: 0 });
    fake.on('sms', 'getMessage', FakeResponse.timeout());
    assert.ok((await rejection(c.sms.getMessage({ path: { messageId: 'm1' } }))) instanceof CfMeetTimeoutError);
  });

  test('IDEMPOTENT: the same Idempotency-Key on every attempt', async () => {
    fake
      .on('calling', 'holdCall', FakeResponse.error(502))
      .thenReply(FakeResponse.networkError(), FakeResponse.json({ id: 'c1' }, { idempotentReplayed: true }));
    const r = await client.calling.holdCall({ path: { call_id: 'c1' }, body: { expectedVersion: 1 } });
    const keys = fake.requests.map((q) => q.idempotencyKey);
    assert.equal(keys.length, 3);
    assert.equal(new Set(keys).size, 1);
    assert.ok(keys[0]);
    assert.equal(r.idempotencyKey, keys[0]);
    assert.equal(r.idempotentReplayed, true);
    assert.equal(r.attempts, 3);
  });

  test('NO_RETRY: 503 is not retried (one request); 429 is', async () => {
    fake.on('sms', 'createOptOut', FakeResponse.error(503));
    assert.ok((await rejection(client.sms.createOptOut({ body: { to: '+15550100' } }))) instanceof CfMeetUpstreamUnavailableError);
    assert.equal(fake.requests.length, 1);

    fake.on('sms', 'createOptOut', FakeResponse.error(429)).thenReply(FakeResponse.json({ id: 'o1' }, { status: 201 }));
    const r = await client.sms.createOptOut({ body: { to: '+15550100' } });
    assert.equal(r.status, 201);
    assert.equal(fake.requests.length, 3);
  });

  describe('NEVER (paid telecom): exactly one request, whatever happens', () => {
    test('503 is thrown as is', async () => {
      fake.on('sms', 'sendMessage', FakeResponse.error(503));
      assert.ok((await rejection(sendSms())) instanceof CfMeetUpstreamUnavailableError);
      assert.equal(fake.requests.length, 1);
    });

    for (const [label, reply, reason] of [
      ['502', FakeResponse.error(502), 'bad_gateway'],
      ['500', FakeResponse.error(500), 'server_error'],
      ['504', FakeResponse.error(504), 'gateway_timeout'],
      ['a timeout', FakeResponse.timeout(), 'timeout'],
      ['a connection reset', FakeResponse.networkError(), 'network'],
    ] as const) {
      test(`${label} is an UnknownOutcomeError (${reason}) carrying the key`, async () => {
        fake.on('sms', 'sendMessage', reply);
        const e = await rejection(sendSms());
        assert.ok(e instanceof UnknownOutcomeError);
        assert.equal(e.reason, reason);
        assert.equal(e.idempotencyKey, 'order-42');
        assert.equal(fake.requests.length, 1);
        assert.equal(fake.requests[0]?.idempotencyKey, 'order-42');
        assert.deepEqual(clock.sleeps, []);
      });
    }

    test('a failure before sending is a plain network error', async () => {
      fake.on('sms', 'sendMessage', FakeResponse.networkError({ requestSent: false }));
      const e = await rejection(sendSms());
      assert.ok(e instanceof CfMeetNetworkError);
      assert.equal(fake.requests.length, 1);
    });

    test('an abort while waiting is an UnknownOutcomeError (aborted)', async () => {
      fake.on('sms', 'sendMessage', FakeResponse.hang());
      const ac = new AbortController();
      const p = client.sms.sendMessage(
        { body: { from: { numberResourceId: NUMBER }, to: '+15550100', text: 'hi' }, idempotencyKey: 'order-42' },
        { signal: ac.signal },
      );
      await new Promise((r) => setTimeout(r, 0));
      ac.abort();
      const e = await rejection(p);
      assert.ok(e instanceof UnknownOutcomeError);
      assert.equal(e.reason, 'aborted');
    });

    test('a deliberate replay of the key is answered as replayed', async () => {
      fake
        .on('sms', 'sendMessage', FakeResponse.error(504))
        .thenReply(FakeResponse.json({ id: 'sms_1' }, { status: 202, idempotentReplayed: true }));
      const e = await rejection(sendSms());
      assert.ok(e instanceof UnknownOutcomeError && e.idempotencyKey);
      const r = await sendSms(e.idempotencyKey);
      assert.equal(r.idempotentReplayed, true);
      assert.deepEqual(
        fake.requests.map((q) => q.idempotencyKey),
        ['order-42', 'order-42'],
      );
    });
  });

  test('an abort during a retry wait ends the call', async () => {
    const ac = new AbortController();
    const c = new CfMeetClient({
      baseUrl: BASE,
      auth: { type: 'platformCredential', credential: CRED },
      fetch: fake.fetch,
      retry: { sleep: () => (ac.abort(), Promise.resolve()) },
    });
    fake.on('sms', 'getMessage', FakeResponse.error(503));
    const e = await rejection(c.sms.getMessage({ path: { messageId: 'm1' } }, { signal: ac.signal }));
    assert.ok(e instanceof CfMeetAbortError);
    assert.equal(fake.requests.length, 1);
  });
});

describe('pagination', () => {
  test('follows nextCursor across fake pages', async () => {
    const route = fake.onPages('sms', 'listMessages', [[{ id: 'm1' }, { id: 'm2' }], [{ id: 'm3' }], [{ id: 'm4' }]]);
    const ids: string[] = [];
    for await (const m of client.sms.listMessages.all({ query: { limit: 2 } })) ids.push((m as unknown as { id: string }).id);
    assert.deepEqual(ids, ['m1', 'm2', 'm3', 'm4']);
    assert.deepEqual(
      route.requests.map((r) => r.query.cursor),
      [undefined, 'fake_cursor_1', 'fake_cursor_2'],
    );
    assert.ok(route.requests.every((r) => r.query.limit === '2'));
  });

  test('page helpers follow the pagination spec', async () => {
    assert.deepEqual(fakePageBody('calling', 'listQueues', [1], { nextCursor: 'c2' }), { queues: [1], nextCursor: 'c2' });
    fake
      .on('calling', 'listQueues', FakeResponse.page('calling', 'listQueues', [{ id: 'q1' }], { nextCursor: 'n1' }))
      .thenReply(FakeResponse.page('calling', 'listQueues', [{ id: 'q2' }]));
    const ids: string[] = [];
    for await (const q of client.calling.listQueues.all()) ids.push((q as unknown as { id: string }).id);
    assert.deepEqual(ids, ['q1', 'q2']);
    assert.equal(fake.requests.at(-1)?.query.cursor, 'n1');
  });

  test('an unknown cursor is a validation error', async () => {
    fake.onPages('sms', 'listMessages', [[{ id: 'm1' }]]);
    const e = await rejection(client.sms.listMessages({ query: { cursor: 'bogus' } }));
    assert.ok(e instanceof CfMeetValidationError);
    assert.equal(e.code, 'VALIDATION_ERROR');
  });
});

describe('error mapping', () => {
  const cases: [number, string | undefined, new (...a: never[]) => CfMeetApiError, string][] = [
    [400, undefined, CfMeetValidationError, 'VALIDATION_ERROR'],
    [401, undefined, CfMeetAuthenticationError, 'AUTHENTICATION_FAILED'],
    [403, undefined, CfMeetPermissionError, 'INSUFFICIENT_SCOPE'],
    [404, undefined, CfMeetNotFoundError, 'NOT_FOUND'],
    [409, 'STALE_CALL_VERSION', CfMeetConflictError, 'STALE_CALL_VERSION'],
    [429, 'QUOTA_EXCEEDED', CfMeetQuotaExceededError, 'QUOTA_EXCEEDED'],
    [429, undefined, CfMeetRateLimitedError, 'RATE_LIMITED'],
    [431, undefined, CfMeetValidationError, 'HEADERS_TOO_LARGE'],
  ];
  for (const [status, code, type, expected] of cases) {
    test(`${status} ${expected} → ${type.name}`, async () => {
      fake.on(
        'calling',
        'holdCall',
        FakeResponse.error(status, {
          code,
          message: 'refused',
          ...(status === 429 ? { retryAfter: 86_400, quota: { window: 'requests-per-day', level: 'environment' } } : {}),
        }),
      );
      const e = await rejection(client.calling.holdCall({ path: { call_id: 'c1' }, body: { expectedVersion: 1 } }));
      assert.ok(e instanceof type, `${String(e)}`);
      assert.ok(e instanceof CfMeetApiError);
      assert.equal(e.status, status);
      assert.equal(e.code, expected);
      assert.equal(e.message, `${expected}: refused (request ${fake.requests.at(-1)?.requestId})`);
      const id = fake.requests.at(-1)?.requestId;
      assert.equal(e.requestId, id);
      assert.deepEqual(e.body, { error: { code: expected, message: 'refused', requestId: id } });
      if (status === 429) {
        assert.equal(e.retryAfterMs, 86_400_000);
        assert.equal(e.quota?.window, 'requests-per-day');
      }
      assert.equal(fake.requests.length, 1); // nothing here is retried
    });
  }

  test('helpers produce the contract envelope and quota headers', () => {
    assert.deepEqual(fakeErrorEnvelope('RATE_LIMITED', { requestId: 'req_1' }), {
      error: { code: 'RATE_LIMITED', message: 'fake RATE_LIMITED', requestId: 'req_1' },
    });
    assert.equal(fakeErrorCodeFor(504), 'UPSTREAM_TIMEOUT');
    assert.deepEqual(fakeQuotaHeaders({ limitPerDay: 5 }), { 'x-cf-quota-limit-day': '5' });
  });
});

describe('createFakeWebSocket', () => {
  let ws: FakeWebSocketServer;
  beforeEach(() => {
    ws = createFakeWebSocket();
  });
  const rt = () => ({ WebSocket: ws.WebSocket, reconnect: { sleep: clock.sleep } });

  test('Messaging: frames both ways, reconnect with a fresh ticket, stop on 4401', async () => {
    let minted = 0;
    fake
      .on('messaging', 'issueRealtimeTicket')
      .thenReplyWith(() => FakeResponse.json({ ticket: `tkt_${++minted}`, expiresAt: '2026-01-01T00:00:00Z' }, { status: 201 }));
    const conn = client.realtime.messaging({ ...rt(), auth: { type: 'clientToken', token: 'ct_1' } });
    const received: unknown[] = [];
    conn.on('message', (m) => received.push(m.data));

    const s1 = await ws.connection(1);
    assert.equal(conn.state, 'open');
    assert.equal(s1.url, 'wss://api.cfmeet.test/v1/realtime');
    assert.deepEqual(s1.protocols, ['cfmeet.messaging.v1', 'cfmeet.ticket.tkt_1']);
    assert.equal(s1.ticketSubprotocol, 'tkt_1');
    assert.equal(s1.queryTicket, null); // never in the URL
    assert.equal(s1.protocol, 'cfmeet.messaging.v1');
    assert.equal(fake.requests[0]?.headers.authorization, 'CF-Client-Token ct_1');

    s1.message({ type: 'message.created', id: 'msg_1' });
    conn.send({ type: 'ack', id: 'msg_1' });
    assert.deepEqual(received, [{ type: 'message.created', id: 'msg_1' }]);
    assert.deepEqual(s1.sentJson(), [{ type: 'ack', id: 'msg_1' }]);

    s1.serverClose(1011, 'restarting');
    const s2 = await ws.connection(2);
    assert.equal(s2.ticketSubprotocol, 'tkt_2');
    assert.equal(fake.requestsFor('messaging', 'issueRealtimeTicket').length, 2);
    assert.equal(clock.sleeps.length, 1);

    s2.serverClose(4401, 'authorization ended');
    const closed = await conn.closed;
    assert.equal(closed.code, 4401);
    assert.equal(closed.cause, 'terminal_close');
    assert.equal(ws.sockets.length, 2);
    assert.equal(minted, 2);
  });

  test('Meetings: query ticket, resume after the last sequence', async () => {
    let minted = 0;
    fake.on('meetings', 'v2RealtimeTicket').thenReplyWith(() => FakeResponse.json({ ticket: `mt_${++minted}`, cursor: 5 }));
    const conn = client.realtime.meetings('conf_1', { ...rt(), auth: { type: 'userSession', token: 'sess_1' }, cursor: 5 });
    const s1 = await ws.connection(1);
    const u1 = new URL(s1.url);
    assert.equal(u1.pathname, '/api/v2/conferences/conf_1/realtime');
    assert.deepEqual(Object.fromEntries(u1.searchParams), { ticket: 'mt_1', cursor: '5' });
    assert.equal(s1.ticket, 'mt_1');
    assert.deepEqual(s1.protocols, []);

    s1.message({ type: 'event', event: { sequence: 7 } });
    s1.drop(); // no close frame
    const s2 = await ws.connection(2);
    assert.deepEqual(Object.fromEntries(new URL(s2.url).searchParams), { ticket: 'mt_2', cursor: '7' });
    assert.deepEqual(
      fake.requestsFor('meetings', 'v2RealtimeTicket').map((r) => r.pathParams),
      [{ id: 'conf_1' }, { id: 'conf_1' }],
    );
    conn.close();
    const closed = await conn.closed;
    assert.equal(closed.cause, 'client');
    assert.deepEqual(s2.closedByClient, { code: 1000, reason: 'client closed' });
  });

  test('Calling signaling: a fresh credential per connection (4001 refresh)', async () => {
    let n = 0;
    const conn = client.realtime.callingSignaling({ ...rt(), credential: () => `cfsig_${++n}` });
    const s1 = await ws.connection(1);
    assert.equal(s1.bearer, 'cfsig_1');
    assert.deepEqual(s1.protocols, ['cfmeet.signaling.v1', 'bearer.cfsig_1']);
    assert.equal(s1.protocol, 'cfmeet.signaling.v1');
    s1.serverClose(4001, 'credential expired');
    const s2 = await ws.connection(2);
    assert.equal(s2.bearer, 'cfsig_2');
    conn.close();
    await conn.closed;
    assert.equal(fake.requests.length, 0); // no ticket for signaling
  });

  test('scripted handshake failures: transient retried, terminal stops', async () => {
    fake.on('messaging', 'issueRealtimeTicket').thenReplyWith((r) => FakeResponse.json({ ticket: `tkt_${r.index}` }, { status: 201 }));
    ws.refuseNext();
    ws.refuseNext(1006);
    const conn = client.realtime.messaging({ ...rt(), auth: { type: 'clientToken', token: 'ct_1' } });
    const s1 = await ws.connection(1);
    assert.equal(ws.sockets.length, 3);
    assert.equal(s1.ticketSubprotocol, 'tkt_3'); // a fresh ticket per attempt
    assert.equal(clock.sleeps.length, 2);

    ws.refuseNext(4403);
    s1.serverClose(1012);
    const closed = await conn.closed;
    assert.equal(closed.code, 4403);
    assert.equal(closed.cause, 'terminal_close');
    assert.equal(ws.sockets.length, 4);
  });

  test('a refused ticket stops the connection before any handshake', async () => {
    fake.on('messaging', 'issueRealtimeTicket', FakeResponse.error(403));
    const conn = client.realtime.messaging({ ...rt(), auth: { type: 'clientToken', token: 'ct_1' } });
    const closed = await conn.closed;
    assert.equal(closed.cause, 'refused');
    assert.ok(closed.error instanceof CfMeetPermissionError);
    assert.equal(ws.sockets.length, 0);
  });
});
