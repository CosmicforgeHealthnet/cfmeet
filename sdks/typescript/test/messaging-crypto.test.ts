import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, describe, test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

import {
  CfMeetConfigurationError,
  MessagingCryptoError,
  createMessagingCrypto,
  loadMessagingCrypto,
  verifyIntegrity,
  type MessagingCrypto,
  type MessagingCryptoModule,
  type MessagingCryptoWebGlue,
} from '../src/index.js';

const PKG = fileURLToPath(new URL('../../', import.meta.url));
const REPO = join(PKG, '..', '..');
const sri = (b: Uint8Array) => `sha384-${createHash('sha384').update(b).digest('base64')}`;

function fakeModule(overrides: Partial<Record<keyof MessagingCryptoModule, unknown>> = {}): MessagingCryptoModule {
  const fail = (code: string) => () => {
    throw new Error(code);
  };
  return {
    message_aad: () => new Uint8Array([1, 2, 3]),
    inspect_key_package: (_e: Uint8Array, now: bigint) => ({ deviceId: `dev-${now}`, signatureKey: new Uint8Array(32), free() {} }),
    verify_tree_head: () => true,
    verify_inclusion: () => true,
    verify_consistency: () => false,
    encrypt_file: fail('ATTACHMENT_ENCRYPT_FAILED'),
    decrypt_file: fail('ATTACHMENT_TAMPERED'),
    descriptor_info: fail('BAD_DESCRIPTOR'),
    is_descriptor: () => false,
    verify_transfer_request: (_k: Uint8Array, ctx: string[]) => ctx.length === 7,
    comparison_code: () => '123456',
    WebDevice: class {
      static fromState(): never {
        throw new Error('DEVICE_STATE_INVALID');
      }
      readonly deviceId = 'd';
      encrypt(): never {
        throw new Error('UNKNOWN_GROUP');
      }
    },
    ...overrides,
  } as unknown as MessagingCryptoModule;
}

describe('MessagingCrypto adapter (unit)', () => {
  test('wraps the module and maps errors to stable codes', () => {
    const c = createMessagingCrypto(fakeModule());
    assert.deepEqual([...c.messageAad({ tenantId: 't', applicationId: 'a', environmentId: 'e', conversationId: 'c', clientMessageId: 'm', senderDeviceId: 'd' })], [1, 2, 3]);
    assert.equal(c.inspectKeyPackage(new Uint8Array(), 1700000000).deviceId, 'dev-1700000000');
    assert.equal(
      c.verifyTransferRequest({
        targetDeviceKey: new Uint8Array(),
        context: { transferId: '1', tenantId: '2', applicationId: '3', environmentId: '4', principalId: '5', sourceDeviceId: '6', targetDeviceId: '7' },
        transferKey: new Uint8Array(),
        expiresAt: 1,
        signature: new Uint8Array(),
      }),
      true,
    );
    assert.throws(() => c.decryptFile(new Uint8Array(), 'x', new Uint8Array()), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'ATTACHMENT_TAMPERED');
    assert.throws(() => c.restoreDevice('d', new Uint8Array()), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'DEVICE_STATE_INVALID');
    const dev = c.createDevice('d');
    assert.equal(dev.deviceId, 'd');
    assert.throws(() => dev.encrypt(new Uint8Array(), new Uint8Array(), new Uint8Array()), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'UNKNOWN_GROUP');
    assert.throws(() => c.verifyInclusion({ index: 1.5, size: 2, leafData: new Uint8Array(), proof: new Uint8Array(), root: new Uint8Array() }), MessagingCryptoError);
  });

  test('refuses an incompatible module', () => {
    const m = fakeModule() as unknown as Record<string, unknown>;
    delete m.verify_tree_head;
    assert.throws(() => createMessagingCrypto(m as unknown as MessagingCryptoModule), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'CRYPTO_MODULE_INCOMPATIBLE');
  });

  test('loading from bytes verifies the SRI integrity before initializing', async () => {
    const bytes = new TextEncoder().encode('not really wasm');
    let inits = 0;
    const glue = { ...fakeModule(), default: async () => void inits++ } as unknown as MessagingCryptoWebGlue;
    await assert.rejects(
      loadMessagingCrypto({ glue, wasm: { bytes }, integrity: sri(new TextEncoder().encode('other')) }),
      (e: unknown) => e instanceof MessagingCryptoError && e.code === 'CRYPTO_MODULE_INTEGRITY',
    );
    assert.equal(inits, 0);
    await assert.rejects(loadMessagingCrypto({ glue, wasm: { bytes }, integrity: 'md5-abc' }), CfMeetConfigurationError);
    await assert.rejects(loadMessagingCrypto({ glue, wasm: { bytes }, integrity: '' }), CfMeetConfigurationError);
    const c = await loadMessagingCrypto({ glue, wasm: { bytes }, integrity: sri(bytes) });
    assert.equal(inits, 1);
    assert.equal(c.isDescriptor(new Uint8Array()), false);
    assert.equal(await verifyIntegrity(bytes, `sha256-${createHash('sha256').update(bytes).digest('base64')}`), true);
  });

  test('loading from a URL uses the given fetch and still verifies integrity', async () => {
    const bytes = new Uint8Array([0, 97, 115, 109]);
    const glue = { ...fakeModule(), default: async () => undefined } as unknown as MessagingCryptoWebGlue;
    const fetched: string[] = [];
    const fetchImpl = (async (u: string) => {
      fetched.push(u);
      return new Response(bytes);
    }) as unknown as typeof fetch;
    await loadMessagingCrypto({ glue: async () => glue, wasm: { url: 'https://app.example/m.wasm', fetch: fetchImpl }, integrity: sri(bytes) });
    assert.deepEqual(fetched, ['https://app.example/m.wasm']);
  });
});

// ---- the REAL crypto core ----

const UUID = () => randomUUID();

async function roundTrip(c: MessagingCrypto): Promise<void> {
  const binding = { tenantId: UUID(), applicationId: UUID(), environmentId: UUID(), conversationId: UUID(), clientMessageId: UUID(), senderDeviceId: UUID() };
  const aad = c.messageAad(binding);
  assert.ok(aad.length > 0);
  assert.deepEqual(c.messageAad(binding), aad, 'deterministic');
  assert.notDeepEqual(c.messageAad({ ...binding, conversationId: UUID() }), aad);
  assert.throws(() => c.messageAad({ ...binding, tenantId: 'not-a-uuid' }), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'BAD_UUID');

  // Device keys are generated inside the module; the KeyPackage validates and names the device.
  const aliceId = UUID();
  const bobId = UUID();
  const alice = c.createDevice(aliceId);
  const bob = c.createDevice(bobId);
  const info = c.inspectKeyPackage(bob.keyPackage());
  assert.equal(info.deviceId, bobId);
  assert.deepEqual(info.signatureKey, bob.signaturePublicKey);
  assert.throws(() => c.inspectKeyPackage(new Uint8Array([1, 2, 3])), MessagingCryptoError);

  // MLS group: Alice adds Bob, Bob joins from the Welcome, an application message round-trips.
  const group = new TextEncoder().encode(`group-${UUID()}`);
  alice.createGroup(group);
  const kp = bob.keyPackage();
  const packed = new Uint8Array(4 + kp.length);
  new DataView(packed.buffer).setUint32(0, kp.length);
  packed.set(kp, 4);
  const commit = alice.addDevices(group, packed);
  assert.ok(commit.welcome && commit.welcome.length > 0);
  bob.join(group, commit.welcome!);
  assert.deepEqual(new Set(alice.members(group)), new Set([aliceId, bobId]));
  const msgAad = c.messageAad({ ...binding, senderDeviceId: aliceId });
  const ciphertext = alice.encrypt(group, msgAad, new TextEncoder().encode('hello bob'));
  const processed = bob.process(group, ciphertext, msgAad);
  assert.equal(processed.kind, 'APPLICATION');
  assert.equal(new TextDecoder().decode(processed.plaintext), 'hello bob');
  const again = alice.encrypt(group, msgAad, new TextEncoder().encode('second'));
  assert.throws(() => bob.process(group, again, c.messageAad(binding)), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'AAD_MISMATCH');

  // Attachments: encrypt, bind the descriptor to the server id, decrypt; tampering is refused.
  const attachmentId = UUID();
  const file = c.encryptFile(new TextEncoder().encode('file body'), 'text/plain', 'a.txt');
  assert.match(file.sha256, /^[0-9a-f]{64}$/);
  const descriptor = file.descriptorFor(attachmentId);
  assert.equal(c.isDescriptor(descriptor), true);
  const out = c.decryptFile(descriptor, attachmentId, file.ciphertext);
  assert.equal(new TextDecoder().decode(out.plaintext), 'file body');
  assert.equal(out.fileName, 'a.txt');
  const tampered = file.ciphertext.slice();
  tampered[tampered.length - 1]! ^= 1;
  assert.throws(() => c.decryptFile(descriptor, attachmentId, tampered), (e: unknown) => e instanceof MessagingCryptoError && e.code === 'ATTACHMENT_TAMPERED');
}

describe('MessagingCrypto adapter over the real crypto core', () => {
  const tmp: string[] = [];
  after(() => tmp.forEach((d) => rmSync(d, { recursive: true, force: true })));

  // 1. The artifact apps/web ships (committed, content-addressed), loaded the browser way:
  //    --target web glue + .wasm bytes verified against manifest.json's SRI.
  const shipped = join(REPO, 'apps/web/public/messaging-crypto');
  test('shipped browser artifact, loaded with its manifest SRI', { skip: !existsSync(join(shipped, 'manifest.json')) && 'SKIPPED: apps/web/public/messaging-crypto not present' }, async () => {
    const manifest = JSON.parse(readFileSync(join(shipped, 'manifest.json'), 'utf8')) as { files: Record<string, { role: string; integrity: string }> };
    const name = (role: string) => Object.keys(manifest.files).find((k) => manifest.files[k]!.role === role)!;
    const dir = mkdtempSync(join(tmpdir(), 'cfmeet-sdk-glue-'));
    tmp.push(dir);
    copyFileSync(join(shipped, name('glue')), join(dir, 'glue.mjs'));
    const bytes = readFileSync(join(shipped, name('wasm')));
    const integrity = manifest.files[name('wasm')]!.integrity;
    const glueUrl = pathToFileURL(join(dir, 'glue.mjs')).href;
    await assert.rejects(
      loadMessagingCrypto({ glue: () => import(glueUrl), wasm: { bytes: bytes.subarray(1) }, integrity }),
      (e: unknown) => e instanceof MessagingCryptoError && e.code === 'CRYPTO_MODULE_INTEGRITY',
    );
    const c = await loadMessagingCrypto({ glue: () => import(glueUrl) as Promise<MessagingCryptoWebGlue>, wasm: { bytes }, integrity });
    await roundTrip(c);
  });

  // 2. A fresh `wasm-bindgen --target nodejs` / `--target web` build of the crate
  //    (scripts/build-messaging-crypto-test-module.sh; CI job "messaging crypto adapter").
  const built = process.env.CFMEET_MESSAGING_CRYPTO_TEST_MODULE ?? join(PKG, '.wasm-test');
  const have = existsSync(join(built, 'nodejs', 'cf_messaging_crypto_wasm.js'));
  if (!have && process.env.CFMEET_REQUIRE_MESSAGING_CRYPTO === '1') {
    test('fresh build required', () => assert.fail(`no wasm-bindgen build at ${built}`));
  }
  const skip = !have && `SKIPPED: no wasm-bindgen build at ${built} (run npm run build:wasm-test-module)`;
  if (skip) console.log(`# messaging crypto (fresh build): ${skip}`);

  test('fresh --target nodejs build, passed as a module', { skip }, async () => {
    const require = createRequire(import.meta.url);
    const m = require(join(built, 'nodejs', 'cf_messaging_crypto_wasm.js')) as MessagingCryptoModule;
    await roundTrip(await loadMessagingCrypto({ module: m }));
  });

  test('fresh --target web build, from bytes + SRI', { skip }, async () => {
    const bytes = readFileSync(join(built, 'web', 'cf_messaging_crypto_wasm_bg.wasm'));
    const glueUrl = pathToFileURL(join(built, 'web', 'cf_messaging_crypto_wasm.js')).href;
    const c = await loadMessagingCrypto({ glue: () => import(glueUrl) as Promise<MessagingCryptoWebGlue>, wasm: { bytes }, integrity: sri(bytes) });
    await roundTrip(c);
  });

});
