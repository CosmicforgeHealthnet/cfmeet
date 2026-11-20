// CF Meet Platform "Build" (ADR 0062 / 0063): the userToken auth (cache, refresh before
// expiry, one retry after a refusal), the server-side users helpers, and the device
// clients (chat, calls, meetings, push, installation id) against the test fakes.

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  CfMeetAuthenticationError,
  CfMeetClient,
  CfMeetConfigurationError,
  CfMeetDeviceClient,
  CfMeetError,
  InstallationIdStore,
  UserTokenCache,
  createMemoryStorage,
  decodeUserTokenClaims,
  generateInstallationId,
  isUserToken,
  isValidInstallationId,
  type CfMeetDeviceClientConfig,
  type KeyValueStorage,
} from '../src/index.js';
import { FakeResponse, createFakeClock, createFakeFetch, createFakeWebSocket, type FakeFetch } from '../src/testing.js';

const BASE = 'https://api.cfmeet.test';
const CRED = 'cfmk_sbx_0123456789abcdef_unit-test-not-a-real-secret';
const T0 = Date.parse('2026-11-18T10:00:00Z');
const PRINCIPAL = '66666666-7777-4888-9999-aaaaaaaaaaaa';
const ENDPOINT = 'f6f6f6f6-0707-4818-9929-3a3a3a3a3a3a';
const DEVICE = '0d0d0d0d-1111-4222-8333-444444444444';
const CONVERSATION = '11111111-2222-4333-8444-555555555555';
const CONFERENCE = '22222222-3333-4444-8555-666666666666';
const INSTALLATION = 'inst-test-0001';

function b64url(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString('base64url');
}

/** A structurally valid (unsigned) user token expiring at `expMs`. */
function userToken(n: number, expMs: number): string {
  const header = b64url({ alg: 'EdDSA', typ: 'cfmeet-user+jwt', kid: '0123456789abcdef' });
  const payload = b64url({ iss: 'cfmeet.platform-access', aud: 'cfmeet.platform.client', sub: PRINCIPAL, jti: `jti-${n}`, exp: Math.floor(expMs / 1000), grt: { msg: ['read', 'send'], call: ['receive'] } });
  return `cfut_sbx_${header}.${payload}.c2lnbmF0dXJl${n}`;
}

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

/** A clock the test moves by hand. */
function manualNow(start = T0): { now: () => number; advance(ms: number): void } {
  let t = start;
  return { now: () => t, advance: (ms) => void (t += ms) };
}

interface Rig {
  fake: FakeFetch;
  device: CfMeetDeviceClient;
  tokens: string[];
  clock: { now: () => number; advance(ms: number): void };
  sockets: ReturnType<typeof createFakeWebSocket>;
}

function rig(overrides: Partial<CfMeetDeviceClientConfig> = {}): Rig {
  const fake = createFakeFetch();
  const clock = manualNow();
  const tokens: string[] = [];
  const sockets = createFakeWebSocket();
  const sleeper = createFakeClock();
  const device = new CfMeetDeviceClient({
    baseUrl: BASE,
    fetch: fake.fetch,
    retry: { sleep: sleeper.sleep },
    tokenProvider: async () => {
      const t = userToken(tokens.length + 1, clock.now() + 3600_000);
      tokens.push(t);
      return t;
    },
    installationId: INSTALLATION,
    WebSocket: sockets.WebSocket,
    reconnect: { sleep: sleeper.sleep },
    autoRenew: false,
    now: clock.now,
    ...overrides,
  });
  return { fake, device, tokens, clock, sockets };
}

function messagingSession(n: number, now: number, ttlMs = 600_000) {
  return {
    principalId: PRINCIPAL,
    sessionDeviceId: DEVICE,
    sessionDeviceExpiresAt: iso(now + 86_400_000),
    clientToken: `client-token-${n}`,
    clientTokenExpiresAt: iso(now + ttlMs),
  };
}

function callingSession(n: number, now: number, ttlMs = 600_000) {
  return {
    principalId: PRINCIPAL,
    endpointId: ENDPOINT,
    signalingCredential: `cfsig_test${n}`,
    signalingCredentialExpiresAt: iso(now + ttlMs),
  };
}

const TICKET = 'a'.repeat(64);
function ticket() {
  return FakeResponse.json(
    { ticket: TICKET, expiresAt: iso(T0 + 30_000), path: '/v1/realtime', subprotocol: 'cfmeet.messaging.v1', ticketSubprotocolPrefix: 'cfmeet.ticket.' },
    { status: 201 },
  );
}

// ---------------------------------------------------------------------------------------

describe('user token', () => {
  test('claims are decoded without verification; malformed tokens are rejected', () => {
    const t = userToken(1, T0 + 3600_000);
    assert.ok(isUserToken(t));
    assert.equal(decodeUserTokenClaims(t)?.exp, Math.floor((T0 + 3600_000) / 1000));
    assert.equal(decodeUserTokenClaims(t)?.sub, PRINCIPAL);
    assert.equal(decodeUserTokenClaims('cfut_sbx_not-a-jws'), null);
    assert.equal(decodeUserTokenClaims(CRED), null);
    assert.equal(decodeUserTokenClaims(`cfut_prod_${t.slice('cfut_sbx_'.length)}`), null);
    assert.equal(isUserToken(`cfut_sbx_${'a'.repeat(2048)}.b.c`), false);
  });

  test('the cache asks the provider once, again ~60 s before exp, and shares concurrent calls', async () => {
    const clock = manualNow();
    let calls = 0;
    const cache = new UserTokenCache(
      async () => {
        calls++;
        return userToken(calls, clock.now() + 600_000);
      },
      { now: clock.now },
    );
    const [a, b] = await Promise.all([cache.get(), cache.get()]);
    assert.equal(calls, 1);
    assert.equal(a, b);
    clock.advance(539_000); // 61 s before exp
    assert.equal(await cache.get(), a);
    assert.equal(calls, 1);
    clock.advance(2_000); // 59 s before exp
    const c = await cache.get();
    assert.equal(calls, 2);
    assert.notEqual(c, a);
    cache.invalidate(a); // an older token: kept
    assert.equal(await cache.get(), c);
    cache.invalidate(c);
    await cache.get();
    assert.equal(calls, 3);
  });

  test('requests carry Authorization: Bearer cfut_...; the token is reused until it nears expiry', async () => {
    const r = rig();
    r.fake.onRequest('PUT', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.json({ installationId: INSTALLATION, platform: 'FCM', status: 'ACTIVE' }));
    await r.device.push.register({ platform: 'FCM', handle: 'fcm-1' });
    await r.device.push.register({ platform: 'FCM', handle: 'fcm-2' });
    assert.equal(r.tokens.length, 1);
    assert.equal(r.fake.requests[0]!.headers.authorization, `Bearer ${r.tokens[0]}`);
    r.clock.advance(3600_000 - 30_000);
    await r.device.push.register({ platform: 'FCM', handle: 'fcm-3' });
    assert.equal(r.tokens.length, 2);
    assert.equal(r.fake.requests[2]!.headers.authorization, `Bearer ${r.tokens[1]}`);
  });

  for (const code of ['USER_TOKEN_EXPIRED', 'USER_TOKEN_REVOKED', 'AUTHENTICATION_FAILED']) {
    test(`401 ${code}: a fresh token, retried once`, async () => {
      const r = rig();
      r.fake
        .onRequest('PUT', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.error(401, { code }))
        .thenReply(FakeResponse.json({ installationId: INSTALLATION, platform: 'APNS', status: 'ACTIVE' }));
      const reg = await r.device.push.register({ platform: 'APNS', handle: 'apns' });
      assert.equal(reg.status, 'ACTIVE');
      assert.equal(r.tokens.length, 2);
      assert.equal(r.fake.requests[0]!.headers.authorization, `Bearer ${r.tokens[0]}`);
      assert.equal(r.fake.requests[1]!.headers.authorization, `Bearer ${r.tokens[1]}`);
    });
  }

  test('a second 401 is thrown (only one retry); other 401 codes are not retried', async () => {
    const r = rig();
    r.fake.onRequest('PUT', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.error(401, { code: 'USER_TOKEN_REVOKED' }));
    await assert.rejects(r.device.push.register({ platform: 'FCM', handle: 'h' }), CfMeetAuthenticationError);
    assert.equal(r.fake.requests.length, 2);
    assert.equal(r.tokens.length, 2);

    const s = rig();
    s.fake.onRequest('PUT', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.error(401, { code: 'SOMETHING_ELSE' }));
    await assert.rejects(s.device.push.register({ platform: 'FCM', handle: 'h' }), CfMeetAuthenticationError);
    assert.equal(s.fake.requests.length, 1);
  });

  test('a provider returning a platform credential (or garbage) is refused before sending', async () => {
    for (const bad of [CRED, 'not-a-token', '']) {
      const fake = createFakeFetch();
      const device = new CfMeetDeviceClient({ baseUrl: BASE, fetch: fake.fetch, tokenProvider: () => bad, installationId: INSTALLATION });
      await assert.rejects(device.push.register({ platform: 'FCM', handle: 'h' }), CfMeetConfigurationError);
      assert.equal(fake.requests.length, 0);
    }
    assert.throws(() => new CfMeetDeviceClient({ baseUrl: BASE, tokenProvider: undefined as never }), CfMeetConfigurationError);
  });

  test('a userToken client cannot call platform-credential routes (users helpers are server-side)', async () => {
    const r = rig();
    await assert.rejects(r.device.client.users.create({ externalId: 'rider-1' }), CfMeetConfigurationError);
    assert.equal(r.fake.requests.length, 0);
  });
});

// ---------------------------------------------------------------------------------------

describe('users helpers (server-side)', () => {
  const USER = {
    id: PRINCIPAL,
    externalId: 'rider-42',
    displayName: 'Rider 42',
    imageUrl: null,
    status: 'ACTIVE',
    createdAt: '2026-11-18T10:00:00Z',
    updatedAt: '2026-11-18T10:00:00Z',
  };
  function server(): { fake: FakeFetch; client: CfMeetClient } {
    const fake = createFakeFetch();
    const clock = createFakeClock();
    return { fake, client: new CfMeetClient({ baseUrl: BASE, auth: { type: 'platformCredential', credential: CRED }, fetch: fake.fetch, retry: { sleep: clock.sleep } }) };
  }

  test('create / upsert: POST /v1/users with the platform credential and an Idempotency-Key', async () => {
    const { fake, client } = server();
    fake.onRequest('POST', '/v1/users', FakeResponse.json(USER, { status: 201 })).thenReply(FakeResponse.json(USER, { status: 200 }));
    const created = await client.users.create({ externalId: 'rider-42', displayName: 'Rider 42' }, { idempotencyKey: 'create-rider-42' });
    assert.equal(created.data.id, PRINCIPAL);
    const req = fake.requests[0]!;
    assert.equal(req.method, 'POST');
    assert.equal(req.path, '/v1/users');
    assert.equal(req.headers.authorization, `Bearer ${CRED}`);
    assert.equal(req.idempotencyKey, 'create-rider-42');
    assert.deepEqual(req.body, { externalId: 'rider-42', displayName: 'Rider 42' });
    const again = await client.users.upsert({ externalId: 'rider-42' });
    assert.equal(again.created, false);
    assert.ok(fake.requests[1]!.idempotencyKey);
  });

  test('create is retried on 503 with the same Idempotency-Key (idempotent on externalId)', async () => {
    const { fake, client } = server();
    fake.onRequest('POST', '/v1/users', FakeResponse.error(503)).thenReply(FakeResponse.json(USER, { status: 201 }));
    const res = await client.users.create({ externalId: 'rider-42' });
    assert.equal(res.attempts, 2);
    assert.equal(fake.requests[0]!.idempotencyKey, fake.requests[1]!.idempotencyKey);
  });

  test('get / list / all / update / delete / revokeSessions', async () => {
    const { fake, client } = server();
    fake.onRequest('GET', '/v1/users/{userId}', FakeResponse.json(USER));
    fake
      .onRequest('GET', '/v1/users', FakeResponse.json({ items: [USER], nextCursor: 'c2' }))
      .thenReply(FakeResponse.json({ items: [{ ...USER, id: ENDPOINT }], nextCursor: null }));
    fake.onRequest('PATCH', '/v1/users/{userId}', FakeResponse.json({ ...USER, status: 'SUSPENDED' }));
    fake.onRequest('DELETE', '/v1/users/{userId}', FakeResponse.empty());
    fake.onRequest('POST', '/v1/users/{userId}/revoke-sessions', FakeResponse.json({ tokensNotBefore: '2026-11-18T10:00:00Z' }));

    assert.equal((await client.users.get(PRINCIPAL)).data.externalId, 'rider-42');
    assert.equal(fake.requests[0]!.path, `/v1/users/${PRINCIPAL}`);

    const ids: string[] = [];
    for await (const u of client.users.all({ limit: 1 })) ids.push(u.id);
    assert.deepEqual(ids, [PRINCIPAL, ENDPOINT]);
    assert.deepEqual(fake.requests[1]!.query, { limit: '1' });
    assert.deepEqual(fake.requests[2]!.query, { limit: '1', cursor: 'c2' });

    const updated = await client.users.update(PRINCIPAL, { status: 'SUSPENDED' });
    assert.equal(updated.data.status, 'SUSPENDED');
    assert.equal(fake.requests[3]!.method, 'PATCH');
    assert.deepEqual(fake.requests[3]!.body, { status: 'SUSPENDED' });

    await client.users.delete(PRINCIPAL);
    assert.equal(fake.requests[4]!.method, 'DELETE');

    const revoked = await client.users.revokeSessions(PRINCIPAL);
    assert.equal(revoked.data.tokensNotBefore, '2026-11-18T10:00:00Z');
    assert.equal(fake.requests[5]!.path, `/v1/users/${PRINCIPAL}/revoke-sessions`);
    assert.equal(fake.requests[5]!.rawBody, null);
  });

  test('findByExternalId filters the list', async () => {
    const { fake, client } = server();
    fake.onRequest('GET', '/v1/users', FakeResponse.json({ items: [], nextCursor: null }));
    assert.equal(await client.users.findByExternalId('nobody'), null);
    assert.deepEqual(fake.requests[0]!.query, { externalId: 'nobody', limit: '1' });
  });

  test('createToken / upsertWithToken send the grants; a redacted replay is an error', async () => {
    const { fake, client } = server();
    const token = userToken(1, T0 + 3600_000);
    fake.onRequest('POST', '/v1/users/{userId}/tokens', FakeResponse.json({ token, tokenId: DEVICE, expiresAt: iso(T0 + 3600_000) }, { status: 201 }));
    fake
      .onRequest('POST', '/v1/users/tokens', FakeResponse.json({ user: USER, token, tokenId: DEVICE, expiresAt: iso(T0 + 3600_000) }, { status: 201 }))
      .thenReply(FakeResponse.json({ user: USER, tokenRedacted: true, tokenId: DEVICE, expiresAt: iso(T0 + 3600_000) }, { status: 201 }));

    const minted = await client.users.createToken(PRINCIPAL, { grants: { msg: ['read', 'send'], call: ['receive'] }, ttlSeconds: 900 });
    assert.equal(minted.token, token);
    assert.equal(fake.requests[0]!.path, `/v1/users/${PRINCIPAL}/tokens`);
    assert.deepEqual(fake.requests[0]!.body, { grants: { msg: ['read', 'send'], call: ['receive'] }, ttlSeconds: 900 });

    const grants = { msg: ['read', 'send'] as const, mtg: [{ conferenceId: CONFERENCE, role: 'PARTICIPANT' as const }] };
    const up = await client.users.upsertWithToken({ externalId: 'rider-42', displayName: 'Rider 42', grants });
    assert.equal(up.user.id, PRINCIPAL);
    assert.equal(up.token, token);
    assert.deepEqual(fake.requests[1]!.body, { externalId: 'rider-42', displayName: 'Rider 42', grants });

    await assert.rejects(client.users.upsertWithToken({ externalId: 'rider-42', grants }, { idempotencyKey: 'k1' }), /tokenRedacted/);
  });

  test('token mints are not retried on 503 (a replay would not return the token)', async () => {
    const { fake, client } = server();
    fake.onRequest('POST', '/v1/users/tokens', FakeResponse.error(503));
    await assert.rejects(client.users.upsertWithToken({ externalId: 'rider-42', grants: { msg: ['read'] } }));
    assert.equal(fake.requests.length, 1);
  });
});

// ---------------------------------------------------------------------------------------

describe('chat (device)', () => {
  test('connect: user-sessions (user token) → ticket (client token) → WebSocket; events and REST', async () => {
    const r = rig();
    r.fake.onRequest('POST', '/v1/messaging/user-sessions', FakeResponse.json(messagingSession(1, T0), { status: 201 }));
    r.fake.onRequest('POST', '/v1/realtime/tickets', ticket());
    r.fake.onRequest('POST', '/v1/conversations/{conversationId}/messages', FakeResponse.json({ id: DEVICE, sequence: 7 }, { status: 201 }));
    r.fake.onRequest('PUT', '/v1/conversations/{conversationId}/read-cursor', FakeResponse.json({ sequence: 7 }));
    r.fake.onRequest('GET', '/v1/conversations/{conversationId}/messages', FakeResponse.json({ messages: [], nextAfterSequence: null }));

    const chat = await r.device.chat.connect<{ type: string }>();
    const received: string[] = [];
    chat.on('message', ({ data }) => received.push(data.type));
    const opened = new Promise((resolve) => chat.on('open', resolve));

    const exchange = r.fake.requests[0]!;
    assert.equal(exchange.path, '/v1/messaging/user-sessions');
    assert.equal(exchange.headers.authorization, `Bearer ${r.tokens[0]}`);
    assert.deepEqual(exchange.body, { installationId: INSTALLATION });
    assert.equal(chat.principalId, PRINCIPAL);

    const ws = await r.sockets.connection(1);
    await opened;
    const tickets = r.fake.requests.filter((q) => q.path === '/v1/realtime/tickets');
    assert.equal(tickets.length, 1);
    assert.equal(tickets[0]!.headers.authorization, 'CF-Client-Token client-token-1');
    assert.equal(ws.url, 'wss://api.cfmeet.test/v1/realtime');
    assert.deepEqual(ws.protocols, ['cfmeet.messaging.v1', `cfmeet.ticket.${TICKET}`]);
    ws.message({ type: 'message.created' });
    assert.deepEqual(received, ['message.created']);

    await chat.sendMessage(CONVERSATION, 'on my way');
    const send = r.fake.requests.at(-1)!;
    assert.equal(send.path, `/v1/conversations/${CONVERSATION}/messages`);
    assert.equal(send.headers.authorization, 'CF-Client-Token client-token-1');
    assert.equal(send.headers['cf-acting-principal'], PRINCIPAL);
    assert.equal((send.body as { text: string }).text, 'on my way');
    assert.match((send.body as { clientMessageId: string }).clientMessageId, /^[0-9a-f-]{36}$/);

    await chat.markRead(CONVERSATION, 7);
    assert.deepEqual(r.fake.requests.at(-1)!.body, { sequence: 7 });
    assert.equal(r.fake.requests.at(-1)!.method, 'PUT');

    await chat.listMessages(CONVERSATION, { afterSequence: 3, limit: 50 });
    assert.deepEqual(r.fake.requests.at(-1)!.query, { afterSequence: '3', limit: '50' });

    chat.close();
    const info = await chat.closed;
    assert.equal(info.cause, 'client');
    assert.ok(ws.closedByClient);
    assert.equal(r.tokens.length, 1);
  });

  test('the client token is renewed (with sessionDeviceId) before it expires', async () => {
    const r = rig();
    r.fake
      .onRequest('POST', '/v1/messaging/user-sessions', FakeResponse.json(messagingSession(1, T0, 300_000), { status: 201 }))
      .thenReply(FakeResponse.json(messagingSession(2, T0 + 250_000, 300_000), { status: 200 }));
    r.fake.onRequest('GET', '/v1/conversations', FakeResponse.json({ conversations: [], nextCursor: null }));
    const chat = await r.device.chat.connect({ realtime: false });
    const renewed: string[] = [];
    chat.on('renewed', ({ session }) => renewed.push(session.clientToken));
    await chat.listConversations();
    assert.equal(r.fake.requests.at(-1)!.headers.authorization, 'CF-Client-Token client-token-1');
    r.clock.advance(250_000); // 50 s before expiry
    await chat.listConversations();
    const renewal = r.fake.requests.filter((q) => q.path === '/v1/messaging/user-sessions')[1]!;
    assert.deepEqual(renewal.body, { installationId: INSTALLATION, sessionDeviceId: DEVICE });
    assert.equal(r.fake.requests.at(-1)!.headers.authorization, 'CF-Client-Token client-token-2');
    assert.deepEqual(renewed, ['client-token-2']);
    chat.close();
  });

  test('an ended device (404 on renewal) is replaced by a new one; a refused client token is renewed and the call retried', async () => {
    const r = rig();
    r.fake
      .onRequest('POST', '/v1/messaging/user-sessions', FakeResponse.json(messagingSession(1, T0), { status: 201 }))
      .thenReply(FakeResponse.error(404, { code: 'NOT_FOUND' }))
      .thenReply(FakeResponse.json(messagingSession(2, T0), { status: 201 }));
    r.fake
      .onRequest('GET', '/v1/conversations/{conversationId}/unread', FakeResponse.error(401, { code: 'AUTHENTICATION_FAILED' }))
      .thenReply(FakeResponse.json({ unread: 0 }));
    const chat = await r.device.chat.connect({ realtime: false });
    const res = await chat.getUnreadCount(CONVERSATION);
    assert.deepEqual(res.data, { unread: 0 });
    const exchanges = r.fake.requests.filter((q) => q.path === '/v1/messaging/user-sessions');
    assert.equal(exchanges.length, 3);
    assert.deepEqual(exchanges[1]!.body, { installationId: INSTALLATION, sessionDeviceId: DEVICE });
    assert.deepEqual(exchanges[2]!.body, { installationId: INSTALLATION });
    assert.equal(r.fake.requests.at(-1)!.headers.authorization, 'CF-Client-Token client-token-2');
    chat.close();
  });

  test('the realtime channel reconnects with a fresh ticket after a drop', async () => {
    const r = rig();
    r.fake.onRequest('POST', '/v1/messaging/user-sessions', FakeResponse.json(messagingSession(1, T0), { status: 201 }));
    r.fake.onRequest('POST', '/v1/realtime/tickets', ticket());
    const chat = await r.device.chat.connect();
    const ws1 = await r.sockets.connection(1);
    ws1.serverClose(1011);
    const ws2 = await r.sockets.connection(2);
    assert.equal(ws2.ticketSubprotocol, TICKET);
    assert.equal(r.fake.requests.filter((q) => q.path === '/v1/realtime/tickets').length, 2);
    chat.close();
  });

  test('a refused ticket (device ended): one fresh exchange, then a new channel', async () => {
    const r = rig();
    r.fake
      .onRequest('POST', '/v1/messaging/user-sessions', FakeResponse.json(messagingSession(1, T0), { status: 201 }))
      .thenReply(FakeResponse.json(messagingSession(2, T0), { status: 201 }));
    r.fake.onRequest('POST', '/v1/realtime/tickets', FakeResponse.error(401, { code: 'AUTHENTICATION_FAILED' })).thenReply(ticket());
    const chat = await r.device.chat.connect();
    const ws = await r.sockets.connection(1);
    assert.equal(ws.ticketSubprotocol, TICKET);
    const tickets = r.fake.requests.filter((q) => q.path === '/v1/realtime/tickets');
    assert.deepEqual(tickets.map((q) => q.headers.authorization), ['CF-Client-Token client-token-1', 'CF-Client-Token client-token-2']);
    assert.equal(chat.realtime?.state, 'open');
    chat.close();
  });

  test('a refused exchange (no msg grant) rejects connect', async () => {
    const r = rig();
    r.fake.onRequest('POST', '/v1/messaging/user-sessions', FakeResponse.error(403, { code: 'MSG_GRANT_REQUIRED' }));
    await assert.rejects(r.device.chat.connect(), (e: unknown) => e instanceof CfMeetError && (e as { code?: string }).code === 'MSG_GRANT_REQUIRED');
    assert.equal(r.sockets.sockets.length, 0);
  });
});

// ---------------------------------------------------------------------------------------

describe('calls (device)', () => {
  const INVITATION = '5e6f7081-92a3-44b5-86d7-e8f9a0b1c2d3';
  const CALL = '7f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7';
  const SESSION = '9a9a9a9a-8b8b-4c7c-9d6d-5e5e5e5e5e5e';
  function incoming(version: number) {
    return {
      schema: 'cfmeet.calling.realtime-signal',
      schemaVersion: 1,
      eventId: 'sig_0000000000000000000000000001b112',
      type: 'call.incoming',
      callId: CALL,
      callKind: 'VOICE',
      callState: 'DIALING',
      callVersion: version,
      sequence: 14,
      issuedAt: iso(T0),
      correlationId: 'corr_1',
      data: {},
      invitation: { invitationId: INVITATION, participantId: SESSION, status: 'PENDING', purpose: 'CALL', issuedAt: iso(T0), expiresAt: iso(T0 + 45_000), callerDisplayName: 'Ada' },
      target: { endpointId: ENDPOINT, principalId: PRINCIPAL },
    };
  }

  test('connect: user-sessions → signaling (bearer subprotocol); invitation → accept → media credential', async () => {
    const r = rig();
    r.fake.onRequest('POST', '/v1/calling/user-sessions', FakeResponse.json(callingSession(1, T0), { status: 201 }));
    r.fake.onRequest('POST', '/v1/calling/invitations/{invitation_id}/acknowledge', FakeResponse.json({ invitationId: INVITATION, callId: CALL, status: 'RINGING' }));
    r.fake.onRequest('POST', '/v1/calling/invitations/{invitation_id}/accept', FakeResponse.json({ invitationId: INVITATION, sessionId: SESSION, outcome: 'ACCEPTED' }));
    r.fake.onRequest('POST', '/v1/calling/sessions/{session_id}/media-credentials', FakeResponse.json({ sessionId: SESSION, url: 'wss://media.test', token: 'mt', expiresAt: iso(T0 + 60_000) }));

    const calls = await r.device.calls.connect({ kind: 'MOBILE' });
    assert.deepEqual(r.fake.requests[0]!.body, { installationId: INSTALLATION, kind: 'MOBILE' });
    assert.equal(r.fake.requests[0]!.headers.authorization, `Bearer ${r.tokens[0]}`);
    assert.equal(calls.endpointId, ENDPOINT);

    const invitations: string[] = [];
    calls.on('invitation', ({ invitation }) => invitations.push(invitation.invitationId));
    const ws = await r.sockets.connection(1);
    assert.equal(ws.url, 'wss://api.cfmeet.test/v1/calling/signaling');
    assert.deepEqual(ws.protocols, ['cfmeet.signaling.v1', 'bearer.cfsig_test1']);
    ws.message({ type: 'signaling.ready', endpointId: ENDPOINT, principalId: PRINCIPAL, credentialExpiresAt: iso(T0 + 600_000) });
    ws.message(incoming(6));
    ws.message({ ...incoming(8), type: 'call.ringing', invitation: undefined });
    assert.deepEqual(invitations, [INVITATION]);

    await calls.acknowledge(INVITATION);
    assert.equal(r.fake.requests.at(-1)!.headers.authorization, 'Bearer cfsig_test1');
    const accepted = await calls.accept(INVITATION);
    assert.deepEqual(r.fake.requests.at(-1)!.body, { expectedVersion: 8 }); // the latest version seen
    assert.equal(r.fake.requests.at(-1)!.path, `/v1/calling/invitations/${INVITATION}/accept`);
    const media = await calls.mediaCredential(accepted.data.sessionId);
    assert.equal(media.data.url, 'wss://media.test');
    assert.equal(r.fake.requests.at(-1)!.path, `/v1/calling/sessions/${SESSION}/media-credentials`);

    await assert.rejects(calls.decline('00000000-0000-4000-8000-000000000000'), CfMeetConfigurationError);
    calls.close();
    assert.equal((await calls.closed).cause, 'client');
  });

  test('close 4001 (credential expired): the credential is exchanged again and signaling reconnects with it', async () => {
    const r = rig();
    r.fake
      .onRequest('POST', '/v1/calling/user-sessions', FakeResponse.json(callingSession(1, T0), { status: 201 }))
      .thenReply(FakeResponse.json(callingSession(2, T0), { status: 200 }));
    const calls = await r.device.calls.connect();
    const ws1 = await r.sockets.connection(1);
    ws1.serverClose(4001, 'credential expired');
    const ws2 = await r.sockets.connection(2);
    assert.equal(ws2.bearer, 'cfsig_test2');
    assert.equal(r.fake.requests.filter((q) => q.path === '/v1/calling/user-sessions').length, 2);
    calls.close();
  });

  test('the signaling credential is exchanged again before it expires', async () => {
    const r = rig();
    r.fake
      .onRequest('POST', '/v1/calling/user-sessions', FakeResponse.json(callingSession(1, T0, 300_000), { status: 201 }))
      .thenReply(FakeResponse.json(callingSession(2, T0 + 260_000, 300_000), { status: 200 }));
    r.fake.onRequest('GET', '/v1/calling/invitations', FakeResponse.json({ endpointId: ENDPOINT, data: [] }));
    const calls = await r.device.calls.connect({ signaling: false });
    await calls.recoverInvitations();
    assert.equal(r.fake.requests.at(-1)!.headers.authorization, 'Bearer cfsig_test1');
    r.clock.advance(260_000);
    await calls.recoverInvitations();
    assert.equal(r.fake.requests.at(-1)!.headers.authorization, 'Bearer cfsig_test2');
    calls.close();
  });

  test('recovered invitations carry the call version for accept / decline', async () => {
    const r = rig();
    r.fake.onRequest('POST', '/v1/calling/user-sessions', FakeResponse.json(callingSession(1, T0), { status: 201 }));
    r.fake.onRequest('GET', '/v1/calling/invitations', FakeResponse.json({
      endpointId: ENDPOINT,
      data: [{ invitationId: INVITATION, callId: CALL, callKind: 'VIDEO', callState: 'DIALING', callVersion: 3, purpose: 'CALL', status: 'PENDING', actionable: true, issuedAt: iso(T0), expiresAt: iso(T0 + 30_000), terminalReason: null, sessionId: null, callerDisplayName: null }],
    }));
    r.fake.onRequest('POST', '/v1/calling/invitations/{invitation_id}/decline', FakeResponse.json({ invitationId: INVITATION, outcome: 'DECLINED' }));
    const calls = await r.device.calls.connect({ signaling: false });
    const [inv] = await calls.recoverInvitations();
    assert.equal(inv!.callVersion, 3);
    await calls.decline(inv!);
    assert.deepEqual(r.fake.requests.at(-1)!.body, { expectedVersion: 3 });
    calls.close();
  });
});

// ---------------------------------------------------------------------------------------

describe('meetings (device)', () => {
  test('join with the user token returns the RTC credential and the participant token', async () => {
    const r = rig();
    const rtc = { provider: 'livekit', url: 'wss://rtc.test', credential: 'lk-jwt', expiresAt: iso(T0 + 600_000) };
    r.fake.onRequest('POST', '/api/v2/conferences/{id}/join', FakeResponse.json({
      conference: { id: CONFERENCE, isLocked: false, meetingSpaceId: DEVICE, status: 'ACTIVE' },
      participant: { id: PRINCIPAL },
      participantSession: { id: ENDPOINT },
      participantToken: 'pt-1',
      rtc,
      rtcStatus: 'ISSUED',
      permissions: {},
      policy: {},
      realtime: {},
      mediaContext: {},
    }));
    r.fake.onRequest('POST', '/api/v2/conferences/{id}/media-credential', FakeResponse.json({ rtc: { ...rtc, credential: 'lk-jwt-2' }, permissions: {}, mediaContext: {} }));

    const joined = await r.device.meetings.join(CONFERENCE, { installationId: INSTALLATION });
    const req = r.fake.requests[0]!;
    assert.equal(req.path, `/api/v2/conferences/${CONFERENCE}/join`);
    assert.equal(req.headers.authorization, `Bearer ${r.tokens[0]}`);
    assert.deepEqual(req.body, {}); // installationId is not part of the join body
    assert.equal(joined.participantToken, 'pt-1');
    assert.deepEqual(joined.rtc, rtc);

    await joined.renewMediaCredential();
    const renew = r.fake.requests[1]!;
    assert.equal(renew.headers['x-participant-token'], 'pt-1');
    assert.equal(renew.headers.authorization, undefined);
    assert.deepEqual(renew.body, { participantSessionId: ENDPOINT });
    assert.equal(joined.rtc?.credential, 'lk-jwt-2');
  });

  test('a conference outside the grants is refused (403 surfaces)', async () => {
    const r = rig();
    r.fake.onRequest('POST', '/api/v2/conferences/{id}/join', FakeResponse.error(403, { code: 'MEETING_ACCESS_DENIED' }));
    await assert.rejects(r.device.meetings.join(CONFERENCE), (e: unknown) => (e as { status?: number }).status === 403);
  });
});

// ---------------------------------------------------------------------------------------

describe('push (device)', () => {
  test('register PUTs the handle; unregister DELETEs one platform or all', async () => {
    const r = rig();
    r.fake.onRequest('PUT', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.json({ installationId: INSTALLATION, platform: 'APNS_VOIP', status: 'ACTIVE' }));
    r.fake.onRequest('DELETE', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.empty());
    const reg = await r.device.push.register({ platform: 'APNS_VOIP', handle: 'voip-token' });
    assert.equal(reg.platform, 'APNS_VOIP');
    assert.equal(r.fake.requests[0]!.path, `/v1/messaging/user-push-registrations/${INSTALLATION}`);
    assert.deepEqual(r.fake.requests[0]!.body, { platform: 'APNS_VOIP', handle: 'voip-token' });
    await r.device.push.unregister({ platform: 'APNS_VOIP' });
    assert.equal(r.fake.requests[1]!.method, 'DELETE');
    assert.deepEqual(r.fake.requests[1]!.query, { platform: 'APNS_VOIP' });
    await r.device.push.unregister();
    assert.deepEqual(r.fake.requests[2]!.query, {});
    await assert.rejects(r.device.push.register({ platform: 'WNS' as never, handle: 'x' }), CfMeetConfigurationError);
    await assert.rejects(r.device.push.register({ platform: 'FCM', handle: '' }), CfMeetConfigurationError);
  });
});

// ---------------------------------------------------------------------------------------

describe('installation id', () => {
  test('generated once, persisted in the storage, reused by the next client', async () => {
    const storage = createMemoryStorage();
    const fake = createFakeFetch();
    fake.onRequest('PUT', '/v1/messaging/user-push-registrations/{installationId}', FakeResponse.json({ status: 'ACTIVE' }));
    const tokenProvider = () => userToken(1, Date.now() + 3600_000);
    const a = new CfMeetDeviceClient({ baseUrl: BASE, fetch: fake.fetch, tokenProvider, storage });
    const [id1, id2] = await Promise.all([a.installationId(), a.installationId()]);
    assert.equal(id1, id2);
    assert.ok(isValidInstallationId(id1));
    assert.equal(storage.values.get('cfmeet.installationId'), id1);
    const b = new CfMeetDeviceClient({ baseUrl: BASE, fetch: fake.fetch, tokenProvider, storage });
    assert.equal(await b.installationId(), id1);
    await b.push.register({ platform: 'FCM', handle: 'h' });
    assert.equal(fake.requests[0]!.path, `/v1/messaging/user-push-registrations/${id1}`);
  });

  test('async (AsyncStorage-like) storage; an invalid stored value is replaced; reset() rotates', async () => {
    const values = new Map<string, string>([['app.install', 'bad id!']]);
    const storage: KeyValueStorage = {
      getItem: async (k) => values.get(k) ?? null,
      setItem: async (k, v) => void values.set(k, v),
    };
    const store = new InstallationIdStore({ storage, key: 'app.install' });
    const id = await store.get();
    assert.ok(isValidInstallationId(id));
    assert.equal(values.get('app.install'), id);
    const next = await store.reset();
    assert.notEqual(next, id);
    assert.equal(await store.get(), next);
  });

  test('generated ids match the contract pattern; a fixed id is validated', () => {
    for (let i = 0; i < 20; i++) assert.match(generateInstallationId(), /^[A-Za-z0-9._-]{8,128}$/);
    assert.throws(() => new CfMeetDeviceClient({ baseUrl: BASE, tokenProvider: () => '', installationId: 'short' }), CfMeetConfigurationError);
    assert.throws(() => new InstallationIdStore({ storage: {} as KeyValueStorage }), CfMeetConfigurationError);
  });
});
