// Shared helpers of the real-stack parity suite (test/integration.test.ts) and the
// cross-SDK interop peer (test/support/interop-peer.ts): the stack environment written by
// `cfmeet-cert-stack up --env-out <file>`, the real Messaging crypto core loaded through
// the SDK's adapter, and MLS device / group steps performed ONLY through the SDK's public
// API (typed accessors through the real API Gateway).

import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import {
  CfMeetClient,
  loadMessagingCrypto,
  type MessagingCrypto,
  type MessagingCryptoWebGlue,
  type WebDevice,
} from '../../src/index.js';

/** sdks/typescript (this file is build-test/test/support/stack.js once compiled). */
export const PKG = fileURLToPath(new URL('../../../', import.meta.url));
export const REPO = join(PKG, '..', '..');

export interface TenantEnv {
  readonly tenantId: string;
  readonly applicationId: string;
  readonly environmentId: string;
  readonly serviceAccountId: string;
  readonly credentialId: string;
  readonly scopes: readonly string[];
  readonly principals: readonly string[];
  readonly organizationId?: string;
  readonly projectId?: string;
}

/** The environment file of `cfmeet-cert-stack up` (certification/src/bin/cfmeet-cert-stack.rs). */
export interface StackEnv {
  readonly gatewayUrl: string;
  readonly credentials: { readonly sandbox: string; readonly otherTenant: string; readonly sandboxLimited: string };
  readonly userSession: string;
  readonly user: { readonly id: string; readonly email: string };
  readonly tenants: {
    readonly sandbox: TenantEnv & {
      readonly organizationId: string;
      readonly projectId: string;
      readonly limited: { readonly scopes: readonly string[] };
      /** Principals reserved per SDK suite (one per Messaging device role, see SDK_PRINCIPAL). */
      readonly sdkPrincipals: { readonly typescript: readonly string[]; readonly dart: readonly string[] };
    };
    readonly otherTenant: TenantEnv;
  };
  readonly limits: { readonly sandboxRequestsPerMinute: number; readonly numberSearchesPerMinute: number };
}

export function readStackEnv(file: string): StackEnv {
  const env = JSON.parse(readFileSync(file, 'utf8')) as StackEnv;
  const need = (ok: boolean, what: string) => {
    if (!ok) throw new Error(`stack env ${file}: ${what} missing (start the stack with cfmeet-cert-stack up --env-out <file>)`);
  };
  need(typeof env.gatewayUrl === 'string', 'gatewayUrl');
  need(/^cfmk_sbx_/.test(env.credentials?.sandbox ?? ''), 'credentials.sandbox');
  need(/^cfmk_sbx_/.test(env.credentials?.otherTenant ?? ''), 'credentials.otherTenant');
  need(/^cfmk_sbx_/.test(env.credentials?.sandboxLimited ?? ''), 'credentials.sandboxLimited');
  need(typeof env.userSession === 'string', 'userSession');
  need((env.tenants?.sandbox?.principals?.length ?? 0) >= 2, 'tenants.sandbox.principals (2)');
  for (const sdk of ['typescript', 'dart'] as const) {
    need((env.tenants?.sandbox?.sdkPrincipals?.[sdk]?.length ?? 0) >= 5, `tenants.sandbox.sdkPrincipals.${sdk} (5)`);
  }
  return env;
}

/** A client of the stack's gateway. Retryable classes honour a Retry-After up to the quota window. */
export function stackClient(env: StackEnv, auth: ConstructorParameters<typeof CfMeetClient>[0]['auth'], extra: Partial<ConstructorParameters<typeof CfMeetClient>[0]> = {}) {
  return new CfMeetClient({
    baseUrl: env.gatewayUrl,
    auth,
    timeoutMs: 20_000,
    maxRetries: 2,
    retry: { maxRetryAfterMs: 61_000 },
    userAgent: 'sdk-parity',
    ...extra,
  });
}

/**
 * Roles of `tenants.sandbox.sdkPrincipals.<sdk>` (Messaging limits device-registration
 * challenges per principal and hour: each role registering devices has its own principal).
 */
export const SDK_PRINCIPAL = { e2eeOwner: 0, e2eeMember: 1, interopOwner: 2, interopPeer: 3, realtime: 4 } as const;

export const credentialAuth = (credential: string) => ({ type: 'platformCredential' as const, credential });

/**
 * Waits for a fresh platform quota window when fewer than `needed` requests remain in the
 * current one (the TypeScript and Dart parity suites share the sandbox environment's
 * per-minute quota). Retryable operations wait out a 429 themselves (Retry-After); a
 * NEVER-class operation must not meet one.
 */
export async function ensureQuotaHeadroom(c: CfMeetClient, needed: number): Promise<void> {
  const r = await c.gateway.getDeveloperPlatformContext();
  const q = r.quota;
  if (q?.remainingThisMinute == null || q.remainingThisMinute >= needed) return;
  await new Promise((done) => setTimeout(done, ((q.resetSeconds ?? 60) + 1) * 1000));
}

// ---- the real crypto core (the browser artifact apps/web ships, verified by its SRI) ----

const tmpDirs: string[] = [];
export function cleanupTmp(): void {
  for (const d of tmpDirs.splice(0)) rmSync(d, { recursive: true, force: true });
}

/** Loads apps/web/public/messaging-crypto (glue + .wasm, checked against manifest.json) through the SDK adapter. */
export async function loadShippedCrypto(): Promise<MessagingCrypto> {
  const shipped = join(REPO, 'apps/web/public/messaging-crypto');
  if (!existsSync(join(shipped, 'manifest.json'))) throw new Error(`the Messaging crypto artifact is missing at ${shipped}`);
  const manifest = JSON.parse(readFileSync(join(shipped, 'manifest.json'), 'utf8')) as { files: Record<string, { role: string; integrity: string }> };
  const name = (role: string) => {
    const n = Object.keys(manifest.files).find((k) => manifest.files[k]!.role === role);
    if (!n) throw new Error(`no ${role} in ${shipped}/manifest.json`);
    return n;
  };
  // The glue is an ES module with a .js name in a non-module directory: import a copy.
  const dir = mkdtempSync(join(tmpdir(), 'cfmeet-sdk-parity-'));
  tmpDirs.push(dir);
  copyFileSync(join(shipped, name('glue')), join(dir, 'glue.mjs'));
  const glueUrl = pathToFileURL(join(dir, 'glue.mjs')).href;
  return loadMessagingCrypto({
    glue: () => import(glueUrl) as Promise<MessagingCryptoWebGlue>,
    wasm: { bytes: readFileSync(join(shipped, name('wasm'))) },
    integrity: manifest.files[name('wasm')]!.integrity,
  });
}

// ---- encoding ----

export const b64url = (b: Uint8Array): string => Buffer.from(b).toString('base64url');
export const unb64url = (s: string): Uint8Array => new Uint8Array(Buffer.from(s, 'base64url'));
export const utf8 = (s: string): Uint8Array => new TextEncoder().encode(s);
export const fromUtf8 = (b: Uint8Array): string => new TextDecoder().decode(b);
export const hexBytes = (h: string): Uint8Array => new Uint8Array(Buffer.from(h, 'hex'));

/** `[u32 big-endian length][bytes]...`: the KeyPackage list format of addDevices. */
export function packByteStrings(items: readonly Uint8Array[]): Uint8Array {
  const out = new Uint8Array(items.reduce((n, i) => n + 4 + i.length, 0));
  const view = new DataView(out.buffer);
  let at = 0;
  for (const i of items) {
    view.setUint32(at, i.length);
    out.set(i, at + 4);
    at += 4 + i.length;
  }
  return out;
}

// ---- Messaging devices and groups, through the SDK ----

/** One principal's device: its keys stay inside the wasm module. */
export interface Member {
  readonly principalId: string;
  readonly deviceId: string;
  readonly device: WebDevice;
}

const acting = (principalId: string) => ({ 'cf-acting-principal': principalId });

/** Revokes every ACTIVE device of `principalId` (leftovers of earlier runs against the same stack). */
export async function revokeDevices(c: CfMeetClient, principalId: string): Promise<number> {
  const dir = await c.messaging.listPrincipalDevices({ path: { principalId }, headers: acting(principalId) });
  let n = 0;
  for (const d of dir.data.devices) {
    if (d.status !== 'ACTIVE') continue;
    await c.messaging.revokeDevice({ path: { deviceId: d.deviceId }, headers: acting(principalId), body: { reason: 'REPLACED' } });
    n++;
  }
  return n;
}

/** Registers a device (challenge -> registration KeyPackage from the bridge) and publishes `packages` KeyPackages. */
export async function registerDevice(c: CfMeetClient, crypto: MessagingCrypto, principalId: string, label: string, packages = 1): Promise<Member> {
  const ch = await c.messaging.createDeviceRegistrationChallenge({ headers: acting(principalId) });
  const device = crypto.createDevice(ch.data.deviceId);
  const kp = device.registrationKeyPackage(ch.data.challengeId, hexBytes(ch.data.nonce));
  const reg = await c.messaging.registerDevice({
    headers: acting(principalId),
    body: { challengeId: ch.data.challengeId, keyPackage: b64url(kp), displayLabel: label },
  });
  if (reg.data.deviceId !== ch.data.deviceId || reg.data.status !== 'ACTIVE') {
    throw new Error(`device registration: ${JSON.stringify(reg.data)}`);
  }
  if (packages > 0) {
    await c.messaging.publishKeyPackages({
      path: { deviceId: ch.data.deviceId },
      headers: acting(principalId),
      body: { keyPackages: Array.from({ length: packages }, () => b64url(device.keyPackage())) },
    });
  }
  return { principalId, deviceId: ch.data.deviceId, device };
}

/** The MLS group id of a PRIVATE_E2EE conversation (server-assigned, base64url). */
export async function groupIdOf(c: CfMeetClient, conversationId: string, principalId: string): Promise<Uint8Array> {
  const conv = await c.messaging.getConversation({ path: { conversationId }, headers: acting(principalId) });
  const gid = conv.data.encryption.mlsGroupId;
  if (!gid) throw new Error(`conversation ${conversationId} has no MLS group id`);
  return unb64url(gid);
}

/**
 * The owner creates the MLS group of `conversationId`, claims one KeyPackage of each device
 * of `others` (already conversation members), commits their addition with a Welcome and
 * submits it to the Delivery Service. Returns the commit's sequence and the group id.
 */
export async function establishGroup(
  c: CfMeetClient,
  conversationId: string,
  owner: Member,
  others: readonly string[],
): Promise<{ readonly groupId: Uint8Array; readonly commitSequence: number; readonly welcomed: readonly string[] }> {
  const groupId = await groupIdOf(c, conversationId, owner.principalId);
  owner.device.createGroup(groupId);
  const claim = await c.messaging.claimKeyPackages({
    path: { conversationId },
    headers: acting(owner.principalId),
    body: { principalIds: [...others] },
  });
  const kps = claim.data.keyPackages;
  if (kps.length === 0) throw new Error('no KeyPackage claimed');
  const out = owner.device.addDevices(groupId, packByteStrings(kps.map((k) => unb64url(k.keyPackage))));
  if (!out.welcome) throw new Error('addDevices produced no Welcome');
  const r = await c.messaging.submitMlsCommit({
    path: { conversationId },
    headers: acting(owner.principalId),
    body: {
      clientMessageId: globalThis.crypto.randomUUID(),
      deviceId: owner.deviceId,
      epoch: Number(out.epoch),
      commit: b64url(out.commit),
      welcome: b64url(out.welcome),
      welcomeDeviceIds: kps.map((k) => k.deviceId),
      removedDeviceIds: [],
    },
  });
  return { groupId, commitSequence: r.data.sequence!, welcomed: kps.map((k) => k.deviceId) };
}

/** A welcomed device joins from its pending Welcome and consumes it. Returns the epoch. */
export async function joinFromWelcome(c: CfMeetClient, m: Member, conversationId: string): Promise<{ readonly groupId: Uint8Array; readonly epoch: bigint }> {
  const w = await c.messaging.listWelcomes({ path: { deviceId: m.deviceId }, headers: acting(m.principalId) });
  const welcome = w.data.welcomes?.find((x) => x.conversationId === conversationId);
  if (!welcome) throw new Error(`no Welcome for conversation ${conversationId}: ${JSON.stringify(w.data)}`);
  const groupId = await groupIdOf(c, conversationId, m.principalId);
  const epoch = m.device.join(groupId, unb64url(welcome.welcome));
  await c.messaging.consumeWelcome({ path: { deviceId: m.deviceId, welcomeId: welcome.welcomeId }, headers: acting(m.principalId) });
  return { groupId, epoch };
}

/** Encrypts `text` under the CF Meet AAD on the bridge and sends it. */
export async function sendEncrypted(
  c: CfMeetClient,
  crypto: MessagingCrypto,
  tenant: TenantEnv,
  conversationId: string,
  m: Member,
  groupId: Uint8Array,
  text: string,
) {
  const clientMessageId = globalThis.crypto.randomUUID();
  const aad = crypto.messageAad({
    tenantId: tenant.tenantId,
    applicationId: tenant.applicationId,
    environmentId: tenant.environmentId,
    conversationId,
    clientMessageId,
    senderDeviceId: m.deviceId,
  });
  const ciphertext = m.device.encrypt(groupId, aad, utf8(text));
  const sent = await c.messaging.sendMessage({
    path: { conversationId },
    headers: acting(m.principalId),
    body: { clientMessageId, deviceId: m.deviceId, epoch: Number(m.device.epoch(groupId)), ciphertext: b64url(ciphertext) },
  });
  return { sent, ciphertext: b64url(ciphertext), clientMessageId };
}

/**
 * Fetches the messages after `afterSequence` and processes the ones of OTHER devices on
 * `m`'s device, verifying the exact CF Meet AAD of each. Returns the decrypted texts.
 */
export async function receiveDecrypted(
  c: CfMeetClient,
  crypto: MessagingCrypto,
  tenant: TenantEnv,
  conversationId: string,
  m: Member,
  groupId: Uint8Array,
  afterSequence: number,
): Promise<{ sequence: number; messageId: string; plaintext: string; senderDeviceId: string; ciphertext: string }[]> {
  const page = await c.messaging.listMessages({ path: { conversationId }, query: { afterSequence }, headers: acting(m.principalId) });
  const out: { sequence: number; messageId: string; plaintext: string; senderDeviceId: string; ciphertext: string }[] = [];
  for (const msg of page.data.messages ?? []) {
    if (msg.lifecycle !== 'ACTIVE' || !msg.e2ee?.ciphertext || msg.senderDeviceId === m.deviceId) continue;
    if (BigInt(msg.e2ee.epoch ?? 0) < m.device.epoch(groupId)) continue; // already merged (e.g. the commit we joined with)
    const expectedAad =
      msg.kind === 'APPLICATION'
        ? crypto.messageAad({
            tenantId: tenant.tenantId,
            applicationId: tenant.applicationId,
            environmentId: tenant.environmentId,
            conversationId,
            clientMessageId: msg.clientMessageId!,
            senderDeviceId: msg.senderDeviceId!,
          })
        : null;
    const r = m.device.process(groupId, unb64url(msg.e2ee.ciphertext), expectedAad);
    if (r.kind === 'APPLICATION') {
      out.push({ sequence: msg.sequence!, messageId: msg.messageId!, plaintext: fromUtf8(r.plaintext), senderDeviceId: msg.senderDeviceId!, ciphertext: msg.e2ee.ciphertext });
    }
  }
  return out;
}
