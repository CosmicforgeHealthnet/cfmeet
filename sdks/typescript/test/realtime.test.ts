import assert from 'node:assert/strict';
import { after, before, beforeEach, describe, test } from 'node:test';

import { CfMeetClient, CfMeetConfigurationError, defaultTerminalClose, type RealtimeConnection } from '../src/index.js';
import { errorEnvelope, json, startServer, waitFor, type TestServer, type WsPeer } from './helpers.js';

const CONF = '0b6f1e52-8d8e-4bde-9a51-6c1e0f2b3a4d';
const FAST = { reconnect: { initialDelayMs: 5, maxDelayMs: 20, maxAttempts: 5 } } as const;

let server: TestServer;
let peers: WsPeer[];
let tickets: number;

before(async () => {
  server = await startServer();
});
after(async () => {
  await server.close();
});
beforeEach(() => {
  peers = [];
  tickets = 0;
  server.requests.length = 0;
  server.upgrades.length = 0;
  server.handler = (q, res) => {
    if (q.url === '/v1/realtime/tickets') {
      tickets++;
      return json(res, 201, {
        ticket: tickets.toString(16).padStart(64, '0'),
        expiresAt: '2026-10-23T00:00:00Z',
        path: '/v1/realtime',
        subprotocol: 'cfmeet.messaging.v1',
        ticketSubprotocolPrefix: 'cfmeet.ticket.',
      });
    }
    if (q.url === `/api/v2/conferences/${CONF}/realtime/ticket`) {
      tickets++;
      return json(res, 200, { ticket: `mt-${tickets}`, expiresAt: 'x', websocketPath: 'ignored', cursor: 0, scope: 'FULL' });
    }
    json(res, 404, {});
  };
  server.onUpgrade = (q) => {
    const offered = String(q.headers['sec-websocket-protocol'] ?? '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    return { protocol: offered[0] };
  };
  server.onConnection = (peer) => peers.push(peer);
});

function closedWith(conn: RealtimeConnection): Promise<{ code: number; cause: string }> {
  return conn.closed.then((c) => ({ code: c.code, cause: c.cause }));
}

describe('messaging realtime', () => {
  test('fresh ticket per connect, sent only as a subprotocol; reconnects; stops on 4401', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'clientToken', token: 'ct.a.b' } });
    const conn = c.realtime.messaging(FAST);
    const messages: unknown[] = [];
    const events: string[] = [];
    conn.on('message', (m) => messages.push(m.data));
    conn.on('open', () => events.push('open'));
    conn.on('reconnecting', () => events.push('reconnecting'));
    conn.on('close', (e) => events.push(`close:${e.code}:${e.willReconnect}`));

    await waitFor(() => peers.length === 1, 5000, 'first connection');
    await waitFor(() => conn.state === 'open');
    const up = server.upgrades[0]!;
    assert.equal(up.url, '/v1/realtime');
    assert.equal(up.headers['sec-websocket-protocol'], `cfmeet.messaging.v1, cfmeet.ticket.${'1'.padStart(64, '0')}`);
    assert.equal(server.requests[0]!.headers.authorization, 'CF-Client-Token ct.a.b');

    peers[0]!.send({ type: 'hello', n: 1 });
    await waitFor(() => messages.length === 1);
    assert.deepEqual(messages[0], { type: 'hello', n: 1 });
    conn.send({ type: 'ping' });
    await waitFor(() => peers[0]!.received.length === 1);
    assert.deepEqual(JSON.parse(peers[0]!.received[0]!), { type: 'ping' });

    // server restart → reconnect with a NEW ticket
    peers[0]!.close(1012, 'restart');
    await waitFor(() => peers.length === 2, 5000, 'reconnect');
    assert.equal(server.upgrades[1]!.headers['sec-websocket-protocol'], `cfmeet.messaging.v1, cfmeet.ticket.${'2'.padStart(64, '0')}`);
    await waitFor(() => conn.state === 'open');

    // abnormal drop → reconnect again
    peers[1]!.drop();
    await waitFor(() => peers.length === 3, 5000, 'reconnect after drop');
    await waitFor(() => conn.state === 'open');

    // authentication failure close → terminal, no reconnect
    peers[2]!.close(4401, 'unauthorized');
    const closed = await closedWith(conn);
    assert.deepEqual(closed, { code: 4401, cause: 'terminal_close' });
    await new Promise((r) => setTimeout(r, 100));
    assert.equal(peers.length, 3);
    assert.equal(tickets, 3);
    assert.ok(events.includes('close:1012:true'));
    assert.ok(events.includes('close:4401:false'));
    for (const u of server.upgrades) assert.ok(!u.url.includes('ticket'), 'no ticket in the URL');
  });

  test('explicit close stops reconnecting', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'clientToken', token: 'ct' } });
    const conn = c.realtime.messaging(FAST);
    await waitFor(() => conn.state === 'open');
    conn.close();
    assert.deepEqual(await closedWith(conn), { code: 1000, cause: 'client' });
    await new Promise((r) => setTimeout(r, 100));
    assert.equal(peers.length, 1);
    assert.equal(conn.state, 'closed');
  });

  test('a refused ticket (401) ends the connection without retrying', async () => {
    server.handler = (_q, res) => errorEnvelope(res, 401, 'AUTHENTICATION_FAILED');
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'clientToken', token: 'ct' } });
    const conn = c.realtime.messaging(FAST);
    const errors: unknown[] = [];
    conn.on('error', (e) => errors.push(e.error));
    assert.equal((await closedWith(conn)).cause, 'refused');
    assert.equal(server.requests.length, 1);
    assert.equal(errors.length, 1);
  });

  test('a transient ticket failure (503) is retried with backoff', async () => {
    let n = 0;
    const base = server.handler;
    server.handler = (q, res) => (++n <= 2 ? errorEnvelope(res, 503, 'SERVICE_UNAVAILABLE') : base(q, res));
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'clientToken', token: 'ct' }, maxRetries: 0 });
    const conn = c.realtime.messaging(FAST);
    await waitFor(() => conn.state === 'open', 5000, 'open after retries');
    conn.close();
  });

  test('gives up after maxAttempts consecutive failures', async () => {
    server.onUpgrade = () => ({ reject: 503 });
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'clientToken', token: 'ct' } });
    const conn = c.realtime.messaging({ reconnect: { initialDelayMs: 1, maxDelayMs: 2, maxAttempts: 3 } });
    assert.equal((await closedWith(conn)).cause, 'reconnect_exhausted');
    assert.equal(server.upgrades.length, 4);
  });

  test('requires a client token', () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: 'jwt' } });
    assert.throws(() => c.realtime.messaging(), CfMeetConfigurationError);
  });
});

describe('meetings realtime', () => {
  test('ticket in the query (contract), cursor resumes after the last event', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: 'jwt-1' } });
    const conn = c.realtime.meetings(CONF, FAST);
    await waitFor(() => peers.length === 1 && conn.state === 'open');
    assert.equal(server.upgrades[0]!.url, `/api/v2/conferences/${CONF}/realtime?ticket=mt-1`);
    assert.equal(server.upgrades[0]!.headers['sec-websocket-protocol'], undefined);
    assert.equal(server.upgrades[0]!.headers.authorization, undefined, 'the session token never goes on the socket');
    assert.equal(server.requests[0]!.headers.authorization, 'Bearer jwt-1');

    peers[0]!.send({ type: 'subscribed', cursor: 10 });
    peers[0]!.send({ type: 'event', event: { sequence: 11, eventType: 'x' } });
    peers[0]!.send({ type: 'event', transient: true, event: { sequence: null, eventType: 'typing' } });
    peers[0]!.send({ type: 'event', event: { sequence: 12, eventType: 'y' } });
    await new Promise((r) => setTimeout(r, 50));
    peers[0]!.close(1011, 'unavailable');
    await waitFor(() => peers.length === 2 && conn.state === 'open', 5000, 'reconnect');
    assert.equal(server.upgrades[1]!.url, `/api/v2/conferences/${CONF}/realtime?ticket=mt-2&cursor=12`);

    peers[1]!.close(4403, 'revoked');
    assert.deepEqual(await closedWith(conn), { code: 4403, cause: 'terminal_close' });
  });

  test('a conference closed normally (1000) is terminal', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'participantToken', token: 'pt' } });
    const conn = c.realtime.meetings(CONF, { ...FAST, cursor: 5 });
    await waitFor(() => peers.length === 1 && conn.state === 'open');
    assert.equal(server.upgrades[0]!.url, `/api/v2/conferences/${CONF}/realtime?ticket=mt-1&cursor=5`);
    assert.equal(server.requests[0]!.headers['x-participant-token'], 'pt');
    peers[0]!.close(1000, 'conference closed');
    assert.equal((await closedWith(conn)).cause, 'terminal_close');
  });

  test('unsafe conference ids are refused', () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'userSession', token: 'jwt' } });
    assert.throws(() => c.realtime.meetings('../x'), CfMeetConfigurationError);
  });
});

describe('calling signaling', () => {
  test('credential as the bearer. subprotocol; 4001 reconnects with a fresh credential from the provider', async () => {
    let n = 0;
    const c = new CfMeetClient({ baseUrl: server.url });
    const conn = c.realtime.callingSignaling({ ...FAST, credential: () => `cfsig_tok${++n}` });
    await waitFor(() => peers.length === 1 && conn.state === 'open');
    assert.equal(server.upgrades[0]!.url, '/v1/calling/signaling');
    assert.equal(server.upgrades[0]!.headers['sec-websocket-protocol'], 'cfmeet.signaling.v1, bearer.cfsig_tok1');
    peers[0]!.close(4001, 'credential expired');
    await waitFor(() => peers.length === 2 && conn.state === 'open', 5000, 'refresh');
    assert.equal(server.upgrades[1]!.headers['sec-websocket-protocol'], 'cfmeet.signaling.v1, bearer.cfsig_tok2');
    conn.close();
    await conn.closed;
  });

  test('with a static credential, 4001 is terminal', async () => {
    const c = new CfMeetClient({ baseUrl: server.url, auth: { type: 'signalingCredential', credential: 'cfsig_static' } });
    const conn = c.realtime.callingSignaling(FAST);
    await waitFor(() => peers.length === 1 && conn.state === 'open');
    peers[0]!.close(4001, 'credential expired');
    assert.deepEqual(await closedWith(conn), { code: 4001, cause: 'terminal_close' });
  });

  test('a platform credential is never accepted as a signaling credential', () => {
    const c = new CfMeetClient({ baseUrl: server.url });
    const conn = c.realtime.callingSignaling({ credential: 'cfmk_sbx_a_b' });
    return conn.closed.then((info) => assert.equal(info.cause, 'refused'));
  });
});

test('default terminal close codes', () => {
  for (const code of [1000, 1008, 4001, 4003, 4401, 4403]) assert.equal(defaultTerminalClose(code), true, String(code));
  for (const code of [1001, 1006, 1011, 1012, 1013]) assert.equal(defaultTerminalClose(code), false, String(code));
});
