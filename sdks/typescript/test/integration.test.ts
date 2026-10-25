// Real-stack SDK parity certification (Part VI §52 / §20.2), gated on CFMEET_STACK_ENV.
//
// Start the stack and point CFMEET_STACK_ENV at the environment file it writes:
//
//   make certification-stack ENV_OUT=/tmp/stack.json      # or: cfmeet-cert-stack up --env-out <file>
//   CFMEET_STACK_ENV=/tmp/stack.json npm test
//
// Every scenario of sdks/testdata/parity-scenarios.json is implemented here AND in the Dart
// SDK's test/stack_integration_test.dart; the first test (which runs without a stack too)
// fails when this suite's set of scenario ids differs from the manifest's. Without
// CFMEET_STACK_ENV the stack scenarios print SKIPPED and pass; with it, any failure fails.
// The cross-SDK scenario runs the Dart SDK's interop peer (`dart` on PATH; the FFI
// library from CFMEET_MESSAGING_CRYPTO_LIB, or built with cargo).

import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { after, describe, test } from 'node:test';

import {
  CfMeetApiError,
  CfMeetAuthenticationError,
  CfMeetClient,
  CfMeetConflictError,
  CfMeetNotFoundError,
  CfMeetPermissionError,
  CfMeetValidationError,
  type MessagingCrypto,
  type OperationSpec,
  type WebSocketConstructorLike,
} from '../src/index.js';
import {
  PKG,
  REPO,
  SDK_PRINCIPAL,
  cleanupTmp,
  credentialAuth,
  ensureQuotaHeadroom,
  establishGroup,
  joinFromWelcome,
  loadShippedCrypto,
  readStackEnv,
  receiveDecrypted,
  registerDevice,
  revokeDevices,
  sendEncrypted,
  stackClient,
  type StackEnv,
} from './support/stack.js';
import { PeerProcess } from './support/peer.js';

interface Scenario {
  readonly id: string;
  readonly description: string;
  readonly expect: Record<string, unknown>;
}
const manifest = JSON.parse(readFileSync(join(PKG, '..', 'testdata', 'parity-scenarios.json'), 'utf8')) as { scenarios: Scenario[] };

const file = process.env.CFMEET_STACK_ENV;
const skip = file ? false : 'SKIPPED: CFMEET_STACK_ENV is not set (no real stack)';
if (skip) console.log(`# real-stack parity: ${skip}`);

let envCache: StackEnv | undefined;
const stack = (): StackEnv => (envCache ??= readStackEnv(file!));
let cryptoCache: Promise<MessagingCrypto> | undefined;
const bridge = (): Promise<MessagingCrypto> => (cryptoCache ??= loadShippedCrypto());

const sandbox = () => stackClient(stack(), credentialAuth(stack().credentials.sandbox));
const session = () => stackClient(stack(), { type: 'userSession', token: stack().userSession });
const acting = (principalId: string) => ({ 'cf-acting-principal': principalId });
/** This suite's principal for a Messaging device role. */
const mine = (role: keyof typeof SDK_PRINCIPAL): string => stack().tenants.sandbox.sdkPrincipals.typescript[SDK_PRINCIPAL[role]]!;

async function rejection(p: Promise<unknown>): Promise<unknown> {
  try {
    await p;
  } catch (e) {
    return e;
  }
  assert.fail('expected the call to be refused');
}

const UNROUTED = (suffix: string): OperationSpec => ({
  domain: 'gateway',
  name: 'unrouted',
  operationId: 'unrouted',
  method: 'GET',
  path: `/v1/sdk-certification-unrouted-${suffix}`,
  protocol: 'REST',
  auth: ['PLATFORM_CREDENTIAL'],
  scope: null,
  pathParams: [],
  query: [],
  headers: [],
  body: 'NONE',
  idempotency: 'NONE',
  retry: 'SAFE',
  paidTelecom: false,
  pagination: null,
  success: [200],
});

/** Creates a PRIVATE_E2EE GROUP conversation owned by `owner` with `members` added. */
async function e2eeConversation(c: CfMeetClient, owner: string, members: readonly string[]): Promise<string> {
  const conv = await c.messaging.createConversation({ headers: acting(owner), body: { contextClass: 'GROUP', securityProfile: 'PRIVATE_E2EE' } });
  assert.equal(conv.status, 201);
  assert.equal(conv.data.securityProfile, 'PRIVATE_E2EE');
  assert.equal(conv.data.encryption.serverCanReadContent, false, 'the server cannot read E2EE content');
  assert.match(conv.data.encryption.mlsGroupId ?? '', /^[A-Za-z0-9_-]{43}$/);
  for (const p of members) {
    const m = await c.messaging.addMember({ path: { conversationId: conv.data.conversationId }, headers: acting(owner), body: { principalId: p } });
    assert.ok(m.status === 201 || m.status === 200);
  }
  return conv.data.conversationId;
}

// ---------------------------------------------------------------------------------------
// The scenarios (ids of sdks/testdata/parity-scenarios.json)
// ---------------------------------------------------------------------------------------

const scenarios: Record<string, () => Promise<void>> = {
  async discovery() {
    const c = new CfMeetClient({ baseUrl: stack().gatewayUrl });
    const r = await c.discovery();
    assert.equal(r.status, 200);
    const d = r.data as { service?: unknown; families?: unknown };
    assert.equal(d.service, 'cfmeet.api-gateway');
    assert.ok(Array.isArray(d.families));
    assert.ok(r.requestId, 'x-request-id is surfaced');
  },

  async 'context.credential'() {
    const env = stack();
    const r = await sandbox().gateway.getDeveloperPlatformContext();
    assert.equal(r.status, 200);
    assert.equal(r.data.tenantId, env.tenants.sandbox.tenantId);
    assert.equal(r.data.applicationId, env.tenants.sandbox.applicationId);
    assert.equal(r.data.environmentId, env.tenants.sandbox.environmentId);
    assert.equal(r.data.serviceAccountId, env.tenants.sandbox.serviceAccountId);
    assert.equal(r.data.authority, 'PLATFORM_ACCESS');
    assert.equal(r.data.forwardedBy, 'cfmeet.api-gateway');
    assert.ok(r.requestId);
    assert.ok(r.correlationId);
  },

  async 'errors.defaultDeny'() {
    const e = await rejection(sandbox().request(UNROUTED(randomUUID().slice(0, 8))));
    assert.ok(e instanceof CfMeetNotFoundError, String(e));
    assert.equal(e.status, 404);
    assert.equal(e.code, 'NOT_FOUND');
    assert.ok(e.requestId);
  },

  async 'errors.invalidCredential'() {
    const forged = `cfmk_sbx_${'0'.repeat(24)}_${'0'.repeat(64)}`;
    const c = stackClient(stack(), credentialAuth(forged), { maxRetries: 0 });
    const e = await rejection(c.gateway.getDeveloperPlatformContext());
    assert.ok(e instanceof CfMeetAuthenticationError, String(e));
    assert.equal(e.status, 401);
    assert.equal(e.code, 'AUTHENTICATION_FAILED');
    assert.ok(e.requestId);
    assert.ok(!String(e).includes(forged) && !JSON.stringify(e).includes(forged), 'the credential never appears in the error');
  },

  async 'errors.missingScope'() {
    const env = stack();
    assert.ok(!env.tenants.sandbox.limited.scopes.includes('calls.read'));
    const c = stackClient(env, credentialAuth(env.credentials.sandboxLimited));
    const e = await rejection(c.calling.listCalls());
    assert.ok(e instanceof CfMeetPermissionError, String(e));
    assert.equal(e.status, 403);
    assert.equal(e.code, 'INSUFFICIENT_SCOPE');
    assert.ok(e.requestId);
    // The credential itself is valid: a route within its scopes answers.
    const ok = await c.developerPlatform.listOrganizations();
    assert.equal(ok.status, 200);
  },

  async 'isolation.otherTenant'() {
    const env = stack();
    const t = env.tenants.sandbox;
    const own = await sandbox().developerPlatform.getOrganization({ path: { organizationId: t.organizationId } });
    assert.equal(own.status, 200);
    assert.equal(own.data.id, t.organizationId);
    const other = stackClient(env, credentialAuth(env.credentials.otherTenant));
    for (const call of [
      () => other.developerPlatform.getOrganization({ path: { organizationId: t.organizationId } }),
      () => other.developerPlatform.getProject({ path: { projectId: t.projectId } }),
      () => other.developerPlatform.getEnvironment({ path: { environmentId: t.environmentId } }),
    ]) {
      const e = await rejection(call());
      assert.ok(e instanceof CfMeetNotFoundError, String(e));
      assert.equal(e.status, 404);
      const leaked = JSON.stringify(e.body ?? null);
      for (const id of [t.organizationId, t.projectId, t.environmentId]) assert.ok(!leaked.includes(id), `leak: ${leaked}`);
    }
  },

  async 'developerPlatform.session.list'() {
    const t = stack().tenants.sandbox;
    const c = session();
    const orgs = await c.developerPlatform.listOrganizations();
    assert.equal(orgs.status, 200);
    assert.ok(orgs.data.items.some((o) => o.id === t.organizationId), JSON.stringify(orgs.data));
    const projects = await c.developerPlatform.listProjects({ path: { organizationId: t.organizationId } });
    assert.equal(projects.status, 200);
    assert.ok(projects.data.items.some((p) => p.id === t.projectId), JSON.stringify(projects.data));
  },

  async 'developerPlatform.session.idempotentCreate'() {
    const t = stack().tenants.sandbox;
    const c = session();
    const idempotencyKey = `sdk-parity-ts-${randomUUID()}`;
    const params = { path: { organizationId: t.organizationId }, body: { displayName: 'SDK parity (TypeScript)' }, idempotencyKey };
    const first = await c.developerPlatform.createProject(params);
    assert.equal(first.status, 201);
    assert.equal(first.idempotencyKey, idempotencyKey);
    assert.equal(first.idempotentReplayed, false);
    const replay = await c.developerPlatform.createProject(params);
    assert.equal(replay.status, 201);
    assert.equal(replay.data.id, first.data.id, 'the replay returns the same project');
    // The Developer Platform replays without an Idempotent-Replayed header today: the SDK
    // reports exactly what the gateway sent.
    assert.equal(replay.idempotentReplayed, replay.headers.get('idempotent-replayed')?.trim().toLowerCase() === 'true');
    const e = await rejection(c.developerPlatform.createProject({ ...params, body: { displayName: 'SDK parity (TypeScript), other body' } }));
    assert.ok(e instanceof CfMeetValidationError, String(e));
    assert.equal(e.status, 422);
    assert.equal(e.code, 'IDEMPOTENCY_KEY_REUSED');
  },

  async 'idempotency.replayHeader'() {
    const t = stack().tenants.sandbox;
    const c = sandbox();
    const idempotencyKey = `sdk-parity-ts-${randomUUID()}`;
    const body = {
      externalContextReference: `sdk-parity-ts-${randomUUID().slice(0, 8)}`,
      participants: t.principals.slice(0, 2).map((principalId) => ({ principalId })),
      allowedChannels: ['VOICE' as const],
      directionPolicy: 'BIDIRECTIONAL' as const,
      expiresAt: new Date(Date.now() + 86_400_000).toISOString(),
    };
    const first = await c.privacyRelay.createContext({ body, idempotencyKey });
    assert.equal(first.status, 201);
    assert.equal(first.idempotencyKey, idempotencyKey);
    assert.equal(first.idempotentReplayed, false);
    const replay = await c.privacyRelay.createContext({ body, idempotencyKey });
    assert.equal(replay.status, 201);
    assert.equal(replay.idempotentReplayed, true, 'Idempotent-Replayed: true is surfaced');
    assert.equal(replay.data.context.contextId, first.data.context.contextId, 'the replay returns the same context');
    const e = await rejection(c.privacyRelay.createContext({ body: { ...body, externalContextReference: `${body.externalContextReference}-x` }, idempotencyKey }));
    assert.ok(e instanceof CfMeetConflictError, String(e));
    assert.equal(e.status, 409);
    assert.equal(e.code, 'IDEMPOTENCY_CONFLICT');
  },

  async 'pagination.cursor'() {
    const t = stack().tenants.sandbox;
    const c = session();
    const created: string[] = [];
    for (const n of [1, 2]) {
      const r = await c.developerPlatform.createProject({ path: { organizationId: t.organizationId }, body: { displayName: `SDK parity page ${n} (TypeScript)` } });
      created.push(r.data.id);
    }
    const params = { path: { organizationId: t.organizationId }, query: { limit: 1 } };
    let pages = 0;
    for await (const page of c.developerPlatform.listProjects.pages(params)) {
      pages++;
      assert.equal(page.status, 200);
      assert.ok(page.data.items.length <= 1, 'limit 1 per page');
    }
    const ids: string[] = [];
    for await (const p of c.developerPlatform.listProjects.all(params)) ids.push(p.id);
    assert.ok(ids.length >= 3 && pages >= ids.length, `${ids.length} items over ${pages} pages`);
    assert.equal(new Set(ids).size, ids.length, 'no item twice');
    for (const id of [t.projectId, ...created]) assert.ok(ids.includes(id), `${id} listed`);
  },

  async 'quota.headers'() {
    const env = stack();
    const r = await sandbox().gateway.getDeveloperPlatformContext();
    assert.ok(r.quota, 'x-cf-quota-* headers are surfaced');
    assert.equal(r.quota.limitPerMinute, env.limits.sandboxRequestsPerMinute);
    assert.equal(typeof r.quota.remainingThisMinute, 'number');
    assert.ok(r.quota.remainingThisMinute! < r.quota.limitPerMinute!, 'this request was counted');
    assert.equal(typeof r.quota.resetSeconds, 'number');
    assert.equal(typeof r.quota.limitPerDay, 'number');
  },

  async 'retry.neverPaidTelecom'() {
    const env = stack();
    const [alice, bob] = env.tenants.sandbox.principals as [string, string];
    let sent = 0;
    const keys: string[] = [];
    const counting = (input: string, init: RequestInit) => {
      if (init.method === 'POST' && new URL(input).pathname === '/v1/calls') {
        sent++;
        keys.push(new Headers(init.headers).get('idempotency-key') ?? '');
      }
      return fetch(input, init);
    };
    // Retries are ON for this client (5 after the first attempt, short backoff).
    const c = stackClient(env, credentialAuth(env.credentials.sandbox), { fetch: counting, maxRetries: 5, retry: { initialDelayMs: 10, maxDelayMs: 50 } });
    await ensureQuotaHeadroom(c, 10); // a 429 here would not be the outcome under test
    assert.equal(c.calling.createCall.spec.retry, 'NEVER');
    assert.equal(c.calling.createCall.spec.paidTelecom, true);
    let endpointId = '';
    for (let i = 0; !endpointId; i++) {
      try {
        const ep = await c.calling.registerEndpoint({
          body: { principalId: alice, endpointKey: `sdk-parity-ts-${randomUUID()}`, endpointKind: 'MOBILE', capabilities: { audio: true, video: true } },
        });
        endpointId = ep.data.endpointId;
      } catch (e) {
        if (i >= 50) throw e; // Calling learns principals from the identity feed (eventually)
        await new Promise((r) => setTimeout(r, 200));
      }
    }
    const e = await rejection(c.calling.createCall({ body: { kind: 'VOICE', initiatorPrincipalId: alice, initiatorEndpointId: endpointId, targetPrincipalIds: [bob] } }));
    assert.ok(e instanceof CfMeetValidationError, String(e));
    assert.equal(e.status, 422);
    assert.equal(e.code, 'CAPABILITY_UNAVAILABLE');
    assert.equal(sent, 1, 'exactly one request for a NEVER-class operation');
    assert.match(keys[0]!, /^[0-9a-f-]{36}$/, 'an Idempotency-Key was generated and sent');
  },

  async 'messaging.e2ee'() {
    const env = stack();
    const t = env.tenants.sandbox;
    const alicePrincipal = mine('e2eeOwner');
    const bobPrincipal = mine('e2eeMember');
    const c = sandbox();
    const crypto = await bridge();
    await revokeDevices(c, alicePrincipal);
    await revokeDevices(c, bobPrincipal);
    const alice = await registerDevice(c, crypto, alicePrincipal, 'sdk-parity typescript A', 0);
    const bob = await registerDevice(c, crypto, bobPrincipal, 'sdk-parity typescript B', 1);
    const cid = await e2eeConversation(c, alicePrincipal, [bobPrincipal]);
    const g = await establishGroup(c, cid, alice, [bobPrincipal]);
    assert.deepEqual(g.welcomed, [bob.deviceId]);
    const joined = await joinFromWelcome(c, bob, cid);
    assert.equal(joined.epoch, alice.device.epoch(g.groupId), 'same epoch on both devices');

    const secret = `parity secret from typescript ${randomUUID()}`;
    const { sent, ciphertext } = await sendEncrypted(c, crypto, t, cid, alice, g.groupId, secret);
    assert.equal(sent.status, 201);
    assert.equal(sent.data.kind, 'APPLICATION');
    assert.equal(sent.data.securityProfile, 'PRIVATE_E2EE');
    assert.equal(sent.data.e2ee?.ciphertext, ciphertext, 'the server stores the bridge ciphertext as is');
    assert.ok(!('text' in sent.data) || (sent.data as { text?: unknown }).text == null, 'no plaintext field');
    assert.ok(!JSON.stringify(sent.data).includes('parity secret'), 'plaintext never in the response');

    const listed = await c.messaging.listMessages({ path: { conversationId: cid }, headers: acting(bobPrincipal) });
    const stored = listed.data.messages?.find((m) => m.messageId === sent.data.messageId);
    assert.equal(stored?.e2ee?.ciphertext, ciphertext, 'stored body is the ciphertext');
    assert.ok(!JSON.stringify(listed.data).includes('parity secret'), 'plaintext never stored');

    const got = await receiveDecrypted(c, crypto, t, cid, bob, g.groupId, g.commitSequence);
    assert.deepEqual(got.map((m) => [m.plaintext, m.senderDeviceId]), [[secret, alice.deviceId]], 'decrypted on the receiving device');

    // Plaintext is refused on an E2EE conversation.
    const e = await rejection(
      c.messaging.sendMessage({ path: { conversationId: cid }, headers: acting(alicePrincipal), body: { clientMessageId: randomUUID(), text: secret } }),
    );
    assert.ok(e instanceof CfMeetValidationError, String(e));
    assert.ok(e.status >= 400 && e.status < 500);
  },

  async 'messaging.e2ee.crossSdk'() {
    const env = stack();
    const t = env.tenants.sandbox;
    const alicePrincipal = mine('interopOwner');
    const bobPrincipal = mine('interopPeer');
    const c = sandbox();
    const crypto = await bridge();
    await revokeDevices(c, alicePrincipal);
    await revokeDevices(c, bobPrincipal);
    // Bob's device is run by the Dart SDK (FFI crypto core) in its own process.
    const peer = await PeerProcess.start('dart', ['run', 'test/support/interop_peer.dart', file!, bobPrincipal], { cwd: join(REPO, 'sdks', 'dart') });
    try {
      assert.equal(peer.sdk, 'dart');
      const alice = await registerDevice(c, crypto, alicePrincipal, 'sdk-parity typescript (interop owner)', 0);
      const cid = await e2eeConversation(c, alicePrincipal, [bobPrincipal]);
      const g = await establishGroup(c, cid, alice, [bobPrincipal]);
      assert.deepEqual(g.welcomed, [peer.deviceId], "the Dart device's KeyPackage was claimed");
      const joined = await peer.request({ cmd: 'join', conversationId: cid });
      assert.equal(BigInt(joined.epoch as number), alice.device.epoch(g.groupId));

      const toDart = `typescript -> dart ${randomUUID()}`;
      const { sent } = await sendEncrypted(c, crypto, t, cid, alice, g.groupId, toDart);
      const atPeer = await peer.request({ cmd: 'receive', conversationId: cid, afterSequence: g.commitSequence });
      assert.deepEqual(
        (atPeer.messages as { plaintext: string; senderDeviceId: string }[]).map((m) => [m.plaintext, m.senderDeviceId]),
        [[toDart, alice.deviceId]],
        'the Dart SDK decrypted the TypeScript message',
      );

      const toTs = `dart -> typescript ${randomUUID()}`;
      const reply = await peer.request({ cmd: 'send', conversationId: cid, text: toTs });
      assert.equal(typeof reply.ciphertext, 'string');
      const got = await receiveDecrypted(c, crypto, t, cid, alice, g.groupId, sent.data.sequence!);
      assert.deepEqual(got.map((m) => [m.plaintext, m.senderDeviceId]), [[toTs, peer.deviceId]], 'the TypeScript SDK decrypted the Dart message');
      assert.equal(await peer.exit(), 0);
    } finally {
      peer.kill();
    }
  },

  async 'realtime.messaging'() {
    const env = stack();
    const principal = mine('realtime');
    await revokeDevices(sandbox(), principal);
    const c = sandbox();
    const device = await registerDevice(c, await bridge(), principal, 'sdk-parity typescript realtime', 0);
    const token = await c.messaging.issueClientToken({ headers: acting(principal), body: { deviceId: device.deviceId } });
    assert.equal(token.data.tokenType, 'CF-Client-Token');
    const device$ = stackClient(env, { type: 'clientToken', token: token.data.token });
    const conn = device$.realtime.messaging<{ type?: string; protocolVersion?: number; requestId?: string }>({ reconnect: false });
    const frames: { type?: string; protocolVersion?: number; requestId?: string }[] = [];
    const waitFor = (type: string) =>
      new Promise<{ type?: string; protocolVersion?: number; requestId?: string }>((resolve, reject) => {
        const seen = frames.find((f) => f.type === type);
        if (seen) return resolve(seen);
        const timer = setTimeout(() => reject(new Error(`no ${type} frame (got ${JSON.stringify(frames)})`)), 15_000);
        const off = conn.on('message', ({ data }) => {
          if (data?.type === type) {
            clearTimeout(timer);
            off();
            resolve(data);
          }
        });
        void conn.closed.then((info) => reject(new Error(`closed before ${type}: ${JSON.stringify(info)}`)));
      });
    conn.on('message', ({ data }) => frames.push(data));
    const opened = new Promise<string>((r) => conn.on('open', ({ protocol }) => r(protocol)));
    const welcome = await waitFor('welcome');
    assert.equal(welcome.protocolVersion, 1);
    assert.equal(await opened, 'cfmeet.messaging.v1', 'the ticket is never echoed');
    conn.send({ type: 'ping', requestId: 'parity-1' });
    const pong = await waitFor('pong');
    assert.equal(pong.requestId, 'parity-1');
    conn.close();
    const closed = await conn.closed;
    assert.equal(closed.cause, 'client');
    assert.equal(conn.state, 'closed');
  },

  async 'realtime.meetings'() {
    const c = session();
    const meeting = await c.meetings.v2CreateInstantMeeting({ body: { title: 'SDK parity (TypeScript)' } });
    assert.ok(meeting.status === 201 || meeting.status === 200);
    const m = meeting.data as unknown as { conference?: { id?: string }; id?: string; conferenceId?: string };
    const conferenceId = m.conference?.id ?? m.conferenceId ?? m.id;
    assert.ok(conferenceId, JSON.stringify(meeting.data));
    const conn = c.realtime.meetings<{ type?: string; scope?: string }>(conferenceId, { reconnect: false });
    const subscribed = await new Promise<{ type?: string; scope?: string }>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('no subscribed frame')), 15_000);
      conn.on('message', ({ data }) => {
        if (data?.type === 'subscribed') {
          clearTimeout(timer);
          resolve(data);
        }
      });
      void conn.closed.then((info) => reject(new Error(`closed before subscribed: ${JSON.stringify(info)}`)));
    });
    assert.equal(subscribed.scope, 'FULL');
    conn.close();
    assert.equal((await conn.closed).cause, 'client');
  },

  async 'realtime.meetings.resume'() {
    const env = stack();
    // The helper's SECOND ticket request is held until the test opens the gate, so an
    // event can be committed while it is disconnected (and before that ticket exists).
    let mints = 0;
    let reconnecting!: () => void;
    const reconnectingNow = new Promise<void>((r) => (reconnecting = r));
    let openGate!: () => void;
    const gate = new Promise<void>((r) => (openGate = r));
    const gated = async (input: string, init: RequestInit) => {
      if (init.method === 'POST' && new URL(input).pathname.endsWith('/realtime/ticket') && ++mints === 2) {
        reconnecting();
        await gate;
      }
      return fetch(input, init);
    };
    const c = stackClient(env, { type: 'userSession', token: env.userSession }, { fetch: gated });
    const meeting = await c.meetings.v2CreateInstantMeeting({ body: { title: 'SDK parity resume (TypeScript)' } });
    const m = meeting.data as unknown as { conference?: { id?: string }; id?: string };
    const id = m.conference?.id ?? m.id!;
    const sockets: WebSocket[] = [];
    const urls: string[] = [];
    class RecordingWebSocket extends WebSocket {
      constructor(url: string, protocols?: string | string[]) {
        super(url, protocols);
        sockets.push(this);
        urls.push(url);
      }
    }
    type Frame = { type?: string; cursor?: number; event?: { eventType?: string; sequence?: number } };
    const frames: Frame[] = [];
    const conn = c.realtime.meetings<Frame>(id, {
      WebSocket: RecordingWebSocket as unknown as WebSocketConstructorLike,
      reconnect: { initialDelayMs: 50, maxDelayMs: 200, maxAttempts: 5 },
    });
    conn.on('message', ({ data }) => frames.push(data));
    const waitFor = async (pred: (f: Frame) => boolean, from: number, what: string, timeoutMs = 30_000): Promise<[Frame, number]> => {
      const deadline = Date.now() + timeoutMs;
      for (;;) {
        const i = frames.findIndex((f, k) => k >= from && pred(f));
        if (i >= 0) return [frames[i]!, i];
        if (Date.now() > deadline || conn.state === 'closed') assert.fail(`no ${what} frame: ${JSON.stringify(frames)}`);
        await new Promise((r) => setTimeout(r, 50));
      }
    };
    await waitFor((f) => f.type === 'subscribed', 0, 'subscribed');
    await c.meetings.v2LockConference({ path: { id } });
    const [locked, at] = await waitFor((f) => f.type === 'event' && f.event?.eventType === 'conference.locked', 0, 'conference.locked');
    const n = locked.event!.sequence!;
    assert.equal(typeof n, 'number');
    sockets.at(-1)!.close(3000, 'parity: simulated drop');
    await reconnectingNow; // the helper reconnects by itself and asks for a fresh ticket
    await c.meetings.v2UnlockConference({ path: { id } });
    openGate();
    const [resubscribed, at2] = await waitFor((f) => f.type === 'subscribed', at + 1, 'second subscribed');
    assert.equal(urls.length, 2, 'one reconnect');
    assert.match(urls[1]!, new RegExp(`[?&]cursor=${n}(&|$)`), 'resumes after the last delivered event');
    assert.equal(resubscribed.cursor, n);
    const [unlocked] = await waitFor((f) => f.type === 'event' && f.event?.eventType === 'conference.unlocked', at2 + 1, 'replayed conference.unlocked');
    assert.equal(unlocked.event!.sequence, n + 1);
    assert.equal(mints, 2, 'a fresh ticket per connection');
    conn.close();
    assert.equal((await conn.closed).cause, 'client');
  },
};

// ---------------------------------------------------------------------------------------

test('parity: implements exactly the scenarios of sdks/testdata/parity-scenarios.json', () => {
  const ids = manifest.scenarios.map((s) => s.id);
  assert.equal(new Set(ids).size, ids.length, 'manifest ids are unique');
  assert.deepEqual(Object.keys(scenarios).sort(), [...ids].sort());
  for (const s of manifest.scenarios) {
    assert.ok(s.description && s.expect && typeof s.expect === 'object', `${s.id}: description and expect`);
  }
});

describe('real stack parity (CFMEET_STACK_ENV)', { skip }, () => {
  after(() => cleanupTmp());
  for (const s of manifest.scenarios) {
    test(`${s.id}: ${s.description}`, { timeout: 240_000 }, async () => {
      const run = scenarios[s.id];
      assert.ok(run, `scenario ${s.id} is not implemented`);
      try {
        await run();
      } catch (e) {
        if (e instanceof CfMeetApiError) {
          throw new Error(`${s.id}: ${e.name} ${e.status} ${e.code} (request ${e.requestId}): ${e.message} ${JSON.stringify(e.body)}`, { cause: e });
        }
        throw e;
      }
    });
  }
});
