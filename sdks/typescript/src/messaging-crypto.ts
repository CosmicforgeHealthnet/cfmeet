// Messaging end-to-end encryption: a typed adapter over the EXISTING CF Meet crypto core.
//
// The SDK implements no MLS or cryptography and ships no WebAssembly, keys or secrets.
// The application provides the wasm-bindgen module built from packages/messaging-crypto/
// wasm (the browser artifact apps/web serves from /messaging-crypto/, pinned by its
// manifest.json SRI, or a `wasm-bindgen --target nodejs` build server-side), and the SDK
// exposes its exports with types and stable error codes.
//
// Device private keys live inside the module's WebDevice objects. In a browser, run the
// module in a dedicated Web Worker (as apps/web does) so they never reach page scripts;
// encrypt exported device state before persisting it.

import { CfMeetConfigurationError, CfMeetError } from './errors.js';

/** An error raised by the crypto core: `code` is its stable code (e.g. `AAD_MISMATCH`), never key material. */
export class MessagingCryptoError extends CfMeetError {
  readonly code: string;
  constructor(code: string, cause?: unknown) {
    super(`messaging crypto: ${code}`, { cause });
    this.code = code;
  }
}

// ---- the wasm-bindgen module's exports (packages/messaging-crypto/wasm/src/lib.rs) ----

interface Freeable {
  free?(): void;
}
export interface KeyPackageInfo extends Freeable {
  readonly deviceId: string;
  readonly signatureKey: Uint8Array;
}
export interface CommitResult extends Freeable {
  readonly commit: Uint8Array;
  readonly welcome: Uint8Array | undefined;
  /** The epoch the commit was created in. */
  readonly epoch: bigint;
}
export interface ProcessResult extends Freeable {
  /** `APPLICATION`, `COMMIT` or `PROPOSAL`. */
  readonly kind: string;
  readonly plaintext: Uint8Array;
  readonly epoch: bigint;
}
export interface EncryptedFile extends Freeable {
  readonly ciphertext: Uint8Array;
  /** Lower-case hex SHA-256 of the ciphertext. */
  readonly sha256: string;
  readonly size: bigint;
  /** The descriptor bound to the server-assigned attachment id (SECRET: travels only inside an MLS message). */
  descriptorFor(attachmentId: string): Uint8Array;
}
export interface DecryptedFile extends Freeable {
  readonly plaintext: Uint8Array;
  readonly mediaType: string;
  readonly fileName: string;
}
export interface DescriptorInfo extends Freeable {
  readonly mediaType: string;
  readonly fileName: string;
  readonly size: bigint;
}
/** One device (its keys, groups and pending transfer key stay inside the module). */
export interface WebDevice extends Freeable {
  readonly deviceId: string;
  readonly signaturePublicKey: Uint8Array;
  /** The complete SECRET state: encrypt it before it leaves memory. */
  exportState(): Uint8Array;
  keyPackage(): Uint8Array;
  registrationKeyPackage(challengeId: string, nonce: Uint8Array): Uint8Array;
  hasGroup(groupId: Uint8Array): boolean;
  createGroup(groupId: Uint8Array): bigint;
  join(groupId: Uint8Array, welcome: Uint8Array): bigint;
  addDevices(groupId: Uint8Array, packedKeyPackages: Uint8Array): CommitResult;
  removeDevices(groupId: Uint8Array, deviceIds: string[]): CommitResult;
  encrypt(groupId: Uint8Array, aad: Uint8Array, plaintext: Uint8Array): Uint8Array;
  process(groupId: Uint8Array, message: Uint8Array, expectedAad?: Uint8Array | null): ProcessResult;
  epoch(groupId: Uint8Array): bigint;
  members(groupId: Uint8Array): string[];
  beginTransfer(): Uint8Array;
  signTransferRequest(ctx: string[], expiresAt: bigint): Uint8Array;
  sealHistory(ctx: string[], transferKey: Uint8Array, history: Uint8Array): Uint8Array;
  openHistory(sourceKey: Uint8Array, ctx: string[], sealed: Uint8Array): Uint8Array;
}
export interface WebDeviceConstructor {
  new (deviceId: string): WebDevice;
  fromState(deviceId: string, state: Uint8Array): WebDevice;
}

/** The exports of the wasm-bindgen module (u64 / i64 arguments and results are bigint). */
export interface MessagingCryptoModule {
  message_aad(tenant: string, application: string, environment: string, conversation: string, clientMessage: string, senderDevice: string): Uint8Array;
  inspect_key_package(encoded: Uint8Array, now: bigint): KeyPackageInfo;
  verify_tree_head(logPublicKey: Uint8Array, logId: string, treeSize: bigint, rootHash: Uint8Array, timestampMicros: bigint, signature: Uint8Array): boolean;
  verify_inclusion(index: bigint, size: bigint, leafData: Uint8Array, proof: Uint8Array, root: Uint8Array): boolean;
  verify_consistency(oldSize: bigint, newSize: bigint, oldRoot: Uint8Array, newRoot: Uint8Array, proof: Uint8Array): boolean;
  encrypt_file(plaintext: Uint8Array, mediaType: string, fileName: string): EncryptedFile;
  decrypt_file(descriptor: Uint8Array, attachmentId: string, ciphertext: Uint8Array): DecryptedFile;
  descriptor_info(descriptor: Uint8Array): DescriptorInfo;
  is_descriptor(bytes: Uint8Array): boolean;
  verify_transfer_request(targetDeviceKey: Uint8Array, ctx: string[], transferKey: Uint8Array, expiresAt: bigint, signature: Uint8Array): boolean;
  comparison_code(ctx: string[], transferKey: Uint8Array, sourceKey: Uint8Array, targetKey: Uint8Array): string;
  readonly WebDevice: WebDeviceConstructor;
}

/** A `wasm-bindgen --target web` glue module: the exports plus its default init function. */
export interface MessagingCryptoWebGlue extends MessagingCryptoModule {
  default(init: { module_or_path: BufferSource | WebAssembly.Module }): Promise<unknown>;
}

const REQUIRED_EXPORTS = [
  'message_aad',
  'inspect_key_package',
  'verify_tree_head',
  'verify_inclusion',
  'verify_consistency',
  'encrypt_file',
  'decrypt_file',
  'descriptor_info',
  'is_descriptor',
  'verify_transfer_request',
  'comparison_code',
  'WebDevice',
] as const;

// ---- the adapter ----

/** The CF Meet binding of one application message (all UUIDs). */
export interface MessageBinding {
  readonly tenantId: string;
  readonly applicationId: string;
  readonly environmentId: string;
  readonly conversationId: string;
  readonly clientMessageId: string;
  readonly senderDeviceId: string;
}

/** Transfer context: the seven ids of a history transfer, in protocol order. */
export interface TransferContext {
  readonly transferId: string;
  readonly tenantId: string;
  readonly applicationId: string;
  readonly environmentId: string;
  readonly principalId: string;
  readonly sourceDeviceId: string;
  readonly targetDeviceId: string;
}

export interface MessagingCrypto {
  /** The raw module (advanced use). */
  readonly module: MessagingCryptoModule;
  messageAad(binding: MessageBinding): Uint8Array;
  /** Validates a published KeyPackage (signature, suite, lifetime at `nowSeconds`). */
  inspectKeyPackage(encoded: Uint8Array, nowSeconds?: number | bigint): { readonly deviceId: string; readonly signatureKey: Uint8Array };
  verifyTreeHead(input: {
    logPublicKey: Uint8Array;
    logId: string;
    treeSize: number | bigint;
    rootHash: Uint8Array;
    timestampMicros: number | bigint;
    signature: Uint8Array;
  }): boolean;
  verifyInclusion(input: { index: number | bigint; size: number | bigint; leafData: Uint8Array; proof: Uint8Array; root: Uint8Array }): boolean;
  verifyConsistency(input: { oldSize: number | bigint; newSize: number | bigint; oldRoot: Uint8Array; newRoot: Uint8Array; proof: Uint8Array }): boolean;
  encryptFile(plaintext: Uint8Array, mediaType: string, fileName: string): EncryptedFile;
  decryptFile(descriptor: Uint8Array, attachmentId: string, ciphertext: Uint8Array): DecryptedFile;
  descriptorInfo(descriptor: Uint8Array): DescriptorInfo;
  isDescriptor(bytes: Uint8Array): boolean;
  verifyTransferRequest(input: {
    targetDeviceKey: Uint8Array;
    context: TransferContext;
    transferKey: Uint8Array;
    expiresAt: number | bigint;
    signature: Uint8Array;
  }): boolean;
  comparisonCode(input: { context: TransferContext; transferKey: Uint8Array; sourceKey: Uint8Array; targetKey: Uint8Array }): string;
  /** Generates a new device (keys generated inside the module). */
  createDevice(deviceId: string): WebDevice;
  /** Restores a device from its decrypted exported state. */
  restoreDevice(deviceId: string, state: Uint8Array): WebDevice;
}

function codeOf(e: unknown): string {
  const m = e instanceof Error ? e.message : typeof e === 'string' ? e : '';
  return /^[A-Z][A-Z0-9_]{1,63}$/.test(m) ? m : 'CRYPTO_ERROR';
}

function guard<T>(f: () => T): T {
  try {
    return f();
  } catch (e) {
    if (e instanceof MessagingCryptoError) throw e;
    throw new MessagingCryptoError(codeOf(e), e);
  }
}

const big = (v: number | bigint, name: string): bigint => {
  if (typeof v === 'bigint') return v;
  if (!Number.isSafeInteger(v)) throw new MessagingCryptoError('BAD_ARGUMENT', new RangeError(`${name} must be an integer`));
  return BigInt(v);
};

const ctx = (c: TransferContext): string[] => [
  c.transferId,
  c.tenantId,
  c.applicationId,
  c.environmentId,
  c.principalId,
  c.sourceDeviceId,
  c.targetDeviceId,
];

/** Errors of every device method become MessagingCryptoError with the core's code. */
function guardDevice(device: WebDevice): WebDevice {
  return new Proxy(device, {
    get(target, prop, receiver) {
      const v = Reflect.get(target, prop, receiver);
      if (typeof v !== 'function') return v;
      return (...args: unknown[]) => guard(() => (v as (...a: unknown[]) => unknown).apply(target, args));
    },
  });
}

/** Wraps an initialized module. */
export function createMessagingCrypto(module: MessagingCryptoModule): MessagingCrypto {
  const missing = REQUIRED_EXPORTS.filter((n) => !(module && n in module));
  if (missing.length) {
    throw new MessagingCryptoError('CRYPTO_MODULE_INCOMPATIBLE', new Error(`missing exports: ${missing.join(', ')}`));
  }
  const m = module;
  return Object.freeze({
    module: m,
    messageAad: (b: MessageBinding) =>
      guard(() => m.message_aad(b.tenantId, b.applicationId, b.environmentId, b.conversationId, b.clientMessageId, b.senderDeviceId)),
    inspectKeyPackage: (encoded: Uint8Array, nowSeconds: number | bigint = Math.floor(Date.now() / 1000)) =>
      guard(() => {
        const info = m.inspect_key_package(encoded, big(nowSeconds, 'nowSeconds'));
        const out = { deviceId: info.deviceId, signatureKey: info.signatureKey };
        info.free?.();
        return out;
      }),
    verifyTreeHead: (i: Parameters<MessagingCrypto['verifyTreeHead']>[0]) =>
      guard(() =>
        m.verify_tree_head(i.logPublicKey, i.logId, big(i.treeSize, 'treeSize'), i.rootHash, big(i.timestampMicros, 'timestampMicros'), i.signature),
      ),
    verifyInclusion: (i: Parameters<MessagingCrypto['verifyInclusion']>[0]) =>
      guard(() => m.verify_inclusion(big(i.index, 'index'), big(i.size, 'size'), i.leafData, i.proof, i.root)),
    verifyConsistency: (i: Parameters<MessagingCrypto['verifyConsistency']>[0]) =>
      guard(() => m.verify_consistency(big(i.oldSize, 'oldSize'), big(i.newSize, 'newSize'), i.oldRoot, i.newRoot, i.proof)),
    encryptFile: (p: Uint8Array, mediaType: string, fileName: string) => guard(() => m.encrypt_file(p, mediaType, fileName)),
    decryptFile: (d: Uint8Array, attachmentId: string, c: Uint8Array) => guard(() => m.decrypt_file(d, attachmentId, c)),
    descriptorInfo: (d: Uint8Array) => guard(() => m.descriptor_info(d)),
    isDescriptor: (b: Uint8Array) => guard(() => m.is_descriptor(b)),
    verifyTransferRequest: (i: Parameters<MessagingCrypto['verifyTransferRequest']>[0]) =>
      guard(() => m.verify_transfer_request(i.targetDeviceKey, ctx(i.context), i.transferKey, big(i.expiresAt, 'expiresAt'), i.signature)),
    comparisonCode: (i: Parameters<MessagingCrypto['comparisonCode']>[0]) =>
      guard(() => m.comparison_code(ctx(i.context), i.transferKey, i.sourceKey, i.targetKey)),
    createDevice: (deviceId: string) => guard(() => guardDevice(new m.WebDevice(deviceId))),
    restoreDevice: (deviceId: string, state: Uint8Array) => guard(() => guardDevice(m.WebDevice.fromState(deviceId, state))),
  });
}

export type MessagingCryptoSource =
  /** An already initialized module (e.g. a `--target nodejs` build, or one the app initialized). */
  | { readonly module: MessagingCryptoModule }
  /** A `--target web` glue module plus the .wasm, verified against its SRI integrity before init. */
  | {
      readonly glue: MessagingCryptoWebGlue | (() => Promise<MessagingCryptoWebGlue>);
      readonly wasm: { readonly bytes: ArrayBuffer | Uint8Array } | { readonly url: string | URL; readonly fetch?: typeof fetch };
      /** The .wasm file's SRI (`sha384-…`, from the artifact's manifest.json). Required. */
      readonly integrity: string;
    };

const SRI_RE = /^(sha256|sha384|sha512)-([A-Za-z0-9+/]+={0,2})$/;

function base64(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

/** Checks bytes against an SRI value (sha256 / sha384 / sha512). */
export async function verifyIntegrity(bytes: Uint8Array, integrity: string): Promise<boolean> {
  const m = SRI_RE.exec(integrity.trim());
  if (!m) throw new CfMeetConfigurationError('integrity must be an SRI value such as sha384-<base64>');
  const subtle = (globalThis as { crypto?: Crypto }).crypto?.subtle;
  if (!subtle) throw new CfMeetConfigurationError('Web Crypto (crypto.subtle) is required to verify the module integrity');
  const alg = { sha256: 'SHA-256', sha384: 'SHA-384', sha512: 'SHA-512' }[m[1] as 'sha256' | 'sha384' | 'sha512'];
  const digest = new Uint8Array(await subtle.digest(alg, bytes));
  return base64(digest) === m[2];
}

/** Loads the application-provided crypto core and returns the typed adapter. */
export async function loadMessagingCrypto(source: MessagingCryptoSource): Promise<MessagingCrypto> {
  if ('module' in source) return createMessagingCrypto(source.module);
  if (!source.integrity) throw new CfMeetConfigurationError('integrity is required when loading the crypto module from bytes or a URL');
  let bytes: Uint8Array;
  if ('bytes' in source.wasm) {
    bytes = source.wasm.bytes instanceof Uint8Array ? source.wasm.bytes : new Uint8Array(source.wasm.bytes);
  } else {
    const f = source.wasm.fetch ?? (globalThis as { fetch?: typeof fetch }).fetch;
    if (!f) throw new CfMeetConfigurationError('no fetch in this runtime: pass wasm.bytes or wasm.fetch');
    const res = await f(String(source.wasm.url), { credentials: 'same-origin', redirect: 'error' });
    if (!res.ok) throw new MessagingCryptoError('CRYPTO_MODULE_UNAVAILABLE', new Error(`HTTP ${res.status}`));
    bytes = new Uint8Array(await res.arrayBuffer());
  }
  if (!(await verifyIntegrity(bytes, source.integrity))) throw new MessagingCryptoError('CRYPTO_MODULE_INTEGRITY');
  const glue = typeof source.glue === 'function' ? await source.glue() : source.glue;
  if (!glue || typeof glue.default !== 'function') {
    throw new MessagingCryptoError('CRYPTO_MODULE_INCOMPATIBLE', new Error('the glue module has no default init export'));
  }
  try {
    await glue.default({ module_or_path: bytes });
  } catch (e) {
    throw new MessagingCryptoError('CRYPTO_MODULE_INIT_FAILED', e);
  }
  return createMessagingCrypto(glue);
}
