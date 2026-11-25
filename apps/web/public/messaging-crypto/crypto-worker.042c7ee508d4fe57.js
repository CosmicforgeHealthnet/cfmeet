// CF Meet Messaging device worker â€” shipped source of public/messaging-crypto/crypto-worker.*.js
// (packaged by scripts/package-messaging-crypto.mjs; do not load this file directly).
//
// ONE Messaging device of ONE signed-in account lives in this dedicated worker:
//
// * The MLS signature key, KeyPackage private keys, every group's epoch secrets, the
//   attachment content keys and the at-rest wrapping key never leave this worker. The
//   page receives public keys, MLS messages / ciphertext and, for display, decrypted
//   message text and files.
// * At rest (IndexedDB) everything is AES-256-GCM encrypted under a NON-EXTRACTABLE Web
//   Crypto key generated here. Script cannot export that key; it protects the stored
//   bytes from being copied out and read elsewhere. It is not a defence against code
//   running in this origin while the user is signed in, or against a compromised browser
//   profile (docs/architecture/web-messaging-client-threat-model.md).
// * The WebAssembly module is fetched, its SHA-384 compared with the value pinned below
//   (the build pins it), and only then compiled â€” under this worker's own CSP, the only
//   place where 'wasm-unsafe-eval' is granted.
// * A device is held by one tab at a time (Web Locks): two tabs advancing one MLS state
//   would fork it.
import init, * as core from "./cf-messaging-crypto.c9bbf742bb7467a9.js";

const WASM_URL = new URL("./cf-messaging-crypto.9698cd171110e953.wasm", import.meta.url);
const WASM_SHA384 = "10ZybdFv2rw9AqSUw/GawqFw/NKfcKyu3Ju9W4ZRAX+ixUx8KWsxPQ8m59tFJrnz";
const DB_NAME = "cfmeet-messaging";
const DB_VERSION = 1;
const MAX_TEXT = 16384;
const MAX_ARCHIVE_BYTES = 512 * 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const enc = new TextEncoder();
const dec = new TextDecoder("utf-8", { fatal: true });

class CodedError extends Error {}
const fail = (code) => {
  throw new CodedError(code);
};
const uuid = (v, what = "BAD_UUID") => (typeof v === "string" && UUID.test(v) ? v : fail(what));
const bytes = (v, what = "BAD_BYTES") => (v instanceof Uint8Array ? v : fail(what));

const b64u = {
  enc(b) {
    let s = "";
    for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
    return btoa(s).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  },
  dec(s) {
    if (typeof s !== "string" || !/^[A-Za-z0-9_-]*$/.test(s)) fail("BAD_BASE64");
    const bin = atob(s.replaceAll("-", "+").replaceAll("_", "/") + "===".slice((s.length + 3) % 4));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  },
};

// ------------------------------------------------------------------ module

let ready = null;
function loadCore() {
  ready ??= (async () => {
    const res = await fetch(WASM_URL, { credentials: "same-origin", redirect: "error" });
    if (!res.ok) fail("CRYPTO_MODULE_UNAVAILABLE");
    const wasmBytes = new Uint8Array(await res.arrayBuffer());
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-384", wasmBytes));
    if (btoa(String.fromCharCode(...digest)) !== WASM_SHA384) fail("CRYPTO_MODULE_INTEGRITY");
    await init({ module_or_path: wasmBytes });
  })();
  return ready;
}

// --------------------------------------------------------------- storage

function req(r) {
  return new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}
function done(tx) {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error ?? new Error("aborted"));
  });
}
let dbp = null;
function db() {
  dbp ??= new Promise((resolve, reject) => {
    const r = indexedDB.open(DB_NAME, DB_VERSION);
    r.onupgradeneeded = () => {
      const d = r.result;
      for (const s of ["keys", "devices", "records", "pending"]) if (!d.objectStoreNames.contains(s)) d.createObjectStore(s);
    };
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
  return dbp;
}

/** AES-256-GCM with the record's identity as AAD (a sealed value cannot be moved). */
async function seal(key, aad, plain) {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: enc.encode(aad) }, key, plain));
  return { v: 1, iv, ct };
}
async function unseal(key, aad, sealed) {
  if (!sealed || sealed.v !== 1) fail("AT_REST_FORMAT");
  try {
    return new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: sealed.iv, additionalData: enc.encode(aad) }, key, sealed.ct));
  } catch {
    fail("AT_REST_TAMPERED");
  }
}
const recordAad = (account, conv, seq) => `cfmeet-msg-record:v1:${account}:${conv}:${seq}`;
const deviceAad = (account) => `cfmeet-msg-device:v1:${account}`;
const pendingAad = (account, cmid) => `cfmeet-msg-pending:v1:${account}:${cmid}`;
const draftAad = (account, conv, cmid) => `cfmeet-msg-draft:v1:${account}:${conv}:${cmid}`;
/** Drafts live in the `pending` store under the account (wipe() erases them with it). */
const draftKey = (account, conv, cmid) => [account, `draft:${conv}:${cmid}`];

// A protected entry replaces the legacy directly usable CryptoKey only after every
// encrypted value has been migrated in one atomic final transaction. Staging contains
// only ciphertext addressed by its original AAD; an abort leaves all originals intact.
const LOCAL_KEY_KIND = "LOCAL_AUTHENTICATOR_V1";
const LOCAL_STAGE_PREFIX = "local-protection:";
function localDescriptor(value, account, scope) {
  if (value?.v !== 1 || value.method !== "WEBAUTHN_PRF" || value.account !== account || value.origin !== self.location.origin) fail("LOCAL_BINDING_MISMATCH");
  for (const field of ["accountDeviceId", "messagingDeviceId"]) uuid(value[field], "LOCAL_BINDING_MISMATCH");
  if (typeof value.installationId !== "string" || !/^[A-Za-z0-9_-]{1,200}$/.test(value.installationId)) fail("LOCAL_BINDING_MISMATCH");
  for (const field of ["tenantId", "applicationId", "environmentId"]) if (uuid(value.scope?.[field]) !== scope[field]) fail("LOCAL_BINDING_MISMATCH");
  if (typeof value.credentialId !== "string" || value.credentialId.length > 2048 || b64u.dec(value.credentialId).length < 1 || b64u.dec(value.prfSalt).length !== 32) fail("LOCAL_BINDING_MISMATCH");
  return { v: 1, method: "WEBAUTHN_PRF", account, credentialId: value.credentialId, prfSalt: value.prfSalt, origin: value.origin,
    installationId: value.installationId, accountDeviceId: value.accountDeviceId, messagingDeviceId: value.messagingDeviceId,
    scope: { tenantId: scope.tenantId, applicationId: scope.applicationId, environmentId: scope.environmentId } };
}
const localAad = (descriptor) => `cfmeet-msg-local-lock:v1:${JSON.stringify(descriptor)}`;
async function localWrappingKey(descriptor, secret) {
  if (bytes(secret).length !== 32) fail("LOCAL_UNLOCK_REFUSED");
  const material = await crypto.subtle.importKey("raw", secret, "HKDF", false, ["deriveKey"]);
  return crypto.subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: b64u.dec(descriptor.prfSalt), info: enc.encode(localAad(descriptor)) }, material,
    { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}
async function restoreDevice() {
  const sealed = await req((await db()).transaction("devices").objectStore("devices").get(ctx.account));
  if (!sealed) return { status: "NONE" };
  const plain = await unseal(ctx.key, deviceAad(ctx.account), sealed);
  let envelope;
  try { envelope = JSON.parse(dec.decode(plain)); } finally { plain.fill(0); }
  const state = b64u.dec(envelope.state);
  try {
    if (ctx.localProtection && ctx.localProtection.descriptor.messagingDeviceId !== envelope.deviceId) fail("LOCAL_BINDING_MISMATCH");
    ctx.device = core.WebDevice.fromState(uuid(envelope.deviceId), state);
  } finally { state.fill(0); }
  ctx.deviceId = envelope.deviceId;
  ctx.meta = { cursors: {}, kt: null, ...envelope.meta };
  ctx.persistent = true;
  return { status: "READY", deviceId: ctx.deviceId, signaturePublicKey: ctx.device.signaturePublicKey,
    ...(ctx.localProtection ? { localProtection: ctx.localProtection.descriptor } : {}) };
}
async function storagePage(storeName, account, after = null, limit = 128) {
  const d = await db();
  const store = d.transaction(storeName).objectStore(storeName);
  const range = IDBKeyRange.bound(after ?? [account], [account, []], after !== null);
  const keys = req(store.getAllKeys(range, limit));
  const values = req(store.getAll(range, limit));
  return { keys: await keys, values: await values };
}
function pendingValueAad(account, key) {
  const match = /^draft:([0-9a-f-]{36}):([0-9a-f-]{36})$/.exec(key);
  return match ? draftAad(account, match[1], match[2]) : pendingAad(account, key);
}
async function discardLocalStages(account) {
  const d = await db();
  const tx = d.transaction("pending", "readwrite");
  tx.objectStore("pending").delete(IDBKeyRange.bound([account, LOCAL_STAGE_PREFIX], [account, LOCAL_STAGE_PREFIX + "\uffff"]));
  await done(tx);
}

// ----------------------------------------------------------------- state

/** The open account: its device, wrapping key, scope and non-secret metadata. */
let ctx = null;
let lockedAccount = null;
const handles = new Map(); // attachment descriptors awaiting their message (never posted out)

function requireDevice() {
  if (!ctx?.device) fail("NO_DEVICE");
  return ctx.device;
}

function acquireLock(account) {
  if (!self.navigator?.locks) return Promise.resolve(true);
  return new Promise((resolve) => {
    self.navigator.locks.request(`cfmeet-messaging-device:${account}`, { ifAvailable: true }, (lock) => {
      if (!lock) {
        resolve(false);
        return undefined;
      }
      resolve(true);
      return new Promise(() => {}); // held for this worker's lifetime
    });
  });
}

async function persistDevice(extraPuts = [], extraDeletes = []) {
  const device = requireDevice();
  if (!ctx.persistent) return;
  // Metadata RPCs from another port may run while a group change awaits HTTP
  // acceptance. Never checkpoint that unconfirmed candidate as durable MLS state.
  const state = ctx.snapshot ?? device.exportState();
  const meta = ctx.snapshot && ctx.membershipSnapshot ? { ...ctx.meta, membership: ctx.membershipSnapshot } : ctx.meta;
  const envelope = enc.encode(JSON.stringify({ deviceId: ctx.deviceId, meta, state: b64u.enc(state) }));
  if (!ctx.snapshot) state.fill(0);
  const sealed = await seal(ctx.key, deviceAad(ctx.account), envelope);
  envelope.fill(0);
  const d = await db();
  const tx = d.transaction(["devices", "records", "pending"], "readwrite");
  tx.objectStore("devices").put(sealed, ctx.account);
  for (const [store, key, value] of extraPuts) tx.objectStore(store).put(value, key);
  for (const [store, key] of extraDeletes) tx.objectStore(store).delete(key);
  await done(tx);
}

function aadFor(conversationId, clientMessageId, senderDeviceId) {
  const s = ctx.scope;
  return core.message_aad(s.tenantId, s.applicationId, s.environmentId, conversationId, clientMessageId, senderDeviceId);
}

// Structured payloads (F4-F7), validated on the way in AND out: only these shapes are ever
// encrypted or shown, every string is bounded, and nothing here is fetched (a link preview
// is built by the sender's device, the server never sees or fetches the link).
const str = (v, max, min = 0) => (typeof v === "string" && v.length >= min && v.length <= max ? v : fail("PAYLOAD_INVALID"));
const optStr = (v, max) => (v === undefined || v === null ? null : str(v, max));
const CF_MEET_ID = /^[A-Za-z0-9._-]{3,40}$/;
function linkPreview(v) {
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "object") fail("PAYLOAD_INVALID");
  const url = str(v.url, 2048, 8);
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    fail("PAYLOAD_INVALID");
  }
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") fail("PAYLOAD_INVALID");
  const result = { url: parsed.href, title: optStr(v.title, 200), description: optStr(v.description, 300), siteName: optStr(v.siteName, 100) };
  if (v.imageDataUrl !== undefined && v.imageDataUrl !== null) {
    const image = str(v.imageDataUrl, 24576, 32);
    // Inline JPEG only: never fetch a recipient's IP-revealing remote image,
    // execute an SVG, or accept an unbounded attachment in a preview.
    if (!/^data:image\/jpeg;base64,\/9j\/[A-Za-z0-9+/]*={0,2}$/.test(image)) fail("PAYLOAD_INVALID");
    result.imageDataUrl = image;
  }
  return result;
}
function coordinate(v, limit) {
  return typeof v === "number" && Number.isFinite(v) && Math.abs(v) <= limit ? Math.round(v * 1e6) / 1e6 : fail("PAYLOAD_INVALID");
}
/** A structured payload (not text / file), normalized; throws PAYLOAD_INVALID otherwise. */
function richPayload(p) {
  switch (p?.t) {
    case "contact":
      if (typeof p.cfMeetId !== "string" || !CF_MEET_ID.test(p.cfMeetId)) fail("PAYLOAD_INVALID");
      return { v: 1, t: "contact", cfMeetId: p.cfMeetId, name: str(p.name, 120, 1) };
    case "location":
      return { v: 1, t: "location", lat: coordinate(p.lat, 90), lng: coordinate(p.lng, 180), label: optStr(p.label, 200) };
    case "poll": {
      if (!Array.isArray(p.options) || p.options.length < 2 || p.options.length > 12) fail("PAYLOAD_INVALID");
      return { v: 1, t: "poll", question: str(p.question, 300, 1), options: p.options.map((o) => str(o, 100, 1)), multiple: p.multiple === true };
    }
    case "vote": {
      if (!Array.isArray(p.choices) || p.choices.length > 12) fail("PAYLOAD_INVALID");
      const choices = [...new Set(p.choices)];
      if (choices.length !== p.choices.length || !choices.every((c) => Number.isInteger(c) && c >= 0 && c < 12)) fail("PAYLOAD_INVALID");
      return { v: 1, t: "vote", poll: uuid(p.poll, "PAYLOAD_INVALID"), choices };
    }
    default:
      return fail("PAYLOAD_INVALID");
  }
}

/** The CF Meet message payload inside MLS (validated before anything is shown). */
function parsePayload(plain) {
  let p;
  try {
    p = JSON.parse(dec.decode(plain));
  } catch {
    fail("PAYLOAD_INVALID");
  }
  if (!p || typeof p !== "object" || p.v !== 1) fail("PAYLOAD_INVALID");
  if (p.t === "text" && typeof p.text === "string" && p.text.length >= 1 && p.text.length <= MAX_TEXT) {
    const preview = linkPreview(p.preview);
    return preview ? { t: "text", text: p.text, preview } : { t: "text", text: p.text };
  }
  if (p.t === "file" && typeof p.d === "string") {
    const descriptor = b64u.dec(p.d);
    if (!core.is_descriptor(descriptor)) fail("PAYLOAD_INVALID");
    return { t: "file", d: p.d, caption: typeof p.caption === "string" ? p.caption.slice(0, 1024) : null };
  }
  const rich = richPayload(p);
  delete rich.v;
  return rich;
}

/** What the page sees of a stored record (never the attachment key). */
function view(r) {
  const out = { ...r };
  if (r.payload?.t === "file") {
    out.payload = { t: "file", caption: r.payload.caption ?? null, attachmentId: r.attachmentId ?? null, name: r.fileName ?? null, mediaType: r.mediaType ?? null };
  }
  return out;
}

/** Deliberate plaintext export; never export the stored MLS state or file descriptor. */
function exportRecord(conversationId, r) {
  const visible = view(r);
  return {
    conversationId, sequence: r.sequence, messageId: r.messageId ?? null,
    clientMessageId: r.clientMessageId ?? null, senderPrincipalId: r.senderPrincipalId ?? null,
    senderDeviceId: r.senderDeviceId ?? null, createdAt: r.createdAt ?? null,
    status: r.status, content: ["SENT", "DECRYPTED"].includes(r.status) ? visible.payload ?? null : null,
    attachmentId: r.attachmentId ?? null, fileName: r.fileName ?? null,
    mediaType: r.mediaType ?? null, size: r.size ?? null,
  };
}

// Authoritative removals are separate encrypted references, not an unbounded device
// metadata map. A removed base can be on another server page, absent locally, or
// older than the current visibility window while its EDIT control remains present.
const removalKey = (conversationId, messageId) => `history-removal:${uuid(conversationId)}:${uuid(messageId)}`;
function removedRecord(record, lifecycle) {
  return { sequence: record.sequence, messageId: record.messageId, kind: record.kind,
    senderPrincipalId: record.senderPrincipalId, senderDeviceId: record.senderDeviceId,
    createdAt: record.createdAt, replyToMessageId: record.replyToMessageId ?? null,
    targetMessageId: record.targetMessageId ?? null, status: "REMOVED", lifecycle };
}
async function removalReference(conversationId, messageId, account = ctx.account, key = ctx.key) {
  const identity = removalKey(conversationId, messageId);
  const sealed = await req((await db()).transaction("pending").objectStore("pending").get([account, identity]));
  if (!sealed) return null;
  const plain = await unseal(key, pendingAad(account, identity), sealed);
  try { return str(JSON.parse(dec.decode(plain)).lifecycle, 40, 1); } finally { plain.fill(0); }
}
async function redactRemovedRecord(conversationId, record, currentPage = null, account = ctx.account, key = ctx.key) {
  if (!record?.messageId) return record;
  const lookup = async id => currentPage?.get(id) ?? await removalReference(conversationId, id, account, key);
  const own = await lookup(record.messageId);
  const target = !own && record.kind === "EDIT" && record.targetMessageId ? await lookup(record.targetMessageId) : null;
  return own || target ? removedRecord(record, own ?? target) : record;
}

async function loadRecord(conversationId, sequence) {
  const d = await db();
  const sealed = await req(d.transaction("records").objectStore("records").get([ctx.account, conversationId, sequence]));
  if (!sealed) return null;
  const plain = await unseal(ctx.key, recordAad(ctx.account, conversationId, sequence), sealed);
  try { return await redactRemovedRecord(conversationId, JSON.parse(dec.decode(plain))); } finally { plain.fill(0); }
}

// Retry references use the existing encrypted pending store: no schema upgrade,
// no unbounded metadata array, and no limit that blocks delivery after many failures.
const retryPrefix = (conversationId) => `retry:${conversationId}:`;
const retryKey = (conversationId, sequence) => `${retryPrefix(conversationId)}${String(sequence).padStart(16, "0")}`;
const retryRange = (conversationId, after = 0) => IDBKeyRange.bound([ctx.account, retryKey(conversationId, after)], [ctx.account, retryPrefix(conversationId) + "\uffff"], true);
async function firstRetry(conversationId, after = 0, ignore = new Set()) {
  const d = await db();
  return new Promise((resolve, reject) => {
    const r = d.transaction("pending").objectStore("pending").openKeyCursor(retryRange(conversationId, after));
    r.onerror = () => reject(r.error);
    r.onsuccess = () => {
      const c = r.result;
      if (!c) return resolve(null);
      const n = Number(c.key[1].slice(retryPrefix(conversationId).length));
      if (ignore.has(n)) c.continue(); else resolve(n);
    };
  });
}
async function retryPut(conversationId, sequence) {
  const key = retryKey(conversationId, sequence);
  return ["pending", [ctx.account, key], await seal(ctx.key, pendingAad(ctx.account, key), enc.encode("{}"))];
}

// V2 history copies contain application records, never MLS state or epoch secrets.
// Every snapshot/chunk/checkpoint remains encrypted under the local account key.
const HISTORY_CHUNK_BYTES = 256 * 1024;
const HISTORY_MAX_CHUNKS = 4096;
const HISTORY_MAX_BYTES = 1024 * 1024 * 1024;
const HISTORY_MANIFEST_INDEX = 0xffffffff;
const historyStateKey = (id, role) => `history-v2:${uuid(id)}:${role}`;
const historyObjectKey = (id, index) => `history-v2:${uuid(id)}:object:${String(index).padStart(10, "0")}`;
async function pendingJson(key) {
  requireDevice();
  const value = await req((await db()).transaction("pending").objectStore("pending").get([ctx.account, key]));
  if (!value) return null;
  const plain = await unseal(ctx.key, pendingAad(ctx.account, key), value);
  try { return JSON.parse(dec.decode(plain)); } finally { plain.fill(0); }
}
async function jsonPut(key, value) {
  const plain = enc.encode(JSON.stringify(value));
  try { return ["pending", [ctx.account, key], await seal(ctx.key, pendingAad(ctx.account, key), plain)]; } finally { plain.fill(0); }
}
function historyContext(value, role) {
  if (!Array.isArray(value) || value.length !== 7) fail("TRANSFER_CONTEXT_INVALID");
  value.forEach((part) => uuid(part, "TRANSFER_CONTEXT_INVALID"));
  const s = ctx.scope;
  if (value[1] !== s.tenantId || value[2] !== s.applicationId || value[3] !== s.environmentId || value[4] !== ctx.account || value[role === "source" ? 5 : 6] !== ctx.deviceId || value[5] === value[6]) fail("TRANSFER_CONTEXT_INVALID");
  return [...value];
}
function historyExpiry(value) {
  if (!Number.isSafeInteger(value) || value <= Math.floor(Date.now() / 1000)) fail("TRANSFER_EXPIRED");
  return value;
}
function historyDescriptor(value) {
  uuid(value?.snapshotId, "ARCHIVE_INVALID"); historyExpiry(value.expiresAt);
  if (!Number.isSafeInteger(value.chunkCount) || value.chunkCount < 1 || value.chunkCount > HISTORY_MAX_CHUNKS ||
    !Number.isSafeInteger(value.totalPlaintextBytes) || value.totalPlaintextBytes < 1 || value.totalPlaintextBytes > HISTORY_MAX_BYTES || b64u.dec(value.manifestDigest).length !== 32) fail("ARCHIVE_INVALID");
  return { snapshotId: value.snapshotId, expiresAt: value.expiresAt, manifestDigest: value.manifestDigest, chunkCount: value.chunkCount, totalPlaintextBytes: value.totalPlaintextBytes };
}
const historyObjectArgs = (state, index) => [state.context, state.descriptor.snapshotId, BigInt(state.descriptor.expiresAt), b64u.dec(state.descriptor.manifestDigest), state.descriptor.chunkCount, BigInt(state.descriptor.totalPlaintextBytes), index];
function visibilityMap(values) {
  if (!Array.isArray(values) || values.length > 10000) fail("HISTORY_VISIBILITY_REQUIRED");
  const out = new Map();
  for (const value of values) {
    uuid(value?.conversationId, "HISTORY_VISIBILITY_REQUIRED");
    if (!Number.isSafeInteger(value.firstSequence) || value.firstSequence < 1 || !Number.isSafeInteger(value.lastSequence) || value.lastSequence < 0 || value.lastSequence < value.firstSequence - 1) fail("HISTORY_VISIBILITY_REQUIRED");
    out.set(value.conversationId, value);
  }
  return out;
}
function eligibleRecord(conversationId, record, visibility) {
  const window = visibility.get(conversationId);
  return window && record.sequence >= window.firstSequence && record.sequence <= window.lastSequence;
}
function historyRecord(record) {
  if (!record || !Number.isSafeInteger(record.sequence) || record.sequence < 1 || !["SENT", "DECRYPTED", "REMOVED"].includes(record.status)) fail("ARCHIVE_INVALID");
  const out = { sequence: record.sequence, messageId: uuid(record.messageId, "ARCHIVE_INVALID"),
    senderPrincipalId: uuid(record.senderPrincipalId, "ARCHIVE_INVALID"), senderDeviceId: uuid(record.senderDeviceId, "ARCHIVE_INVALID"),
    kind: str(record.kind, 40, 1), createdAt: typeof record.createdAt === "string" ? str(record.createdAt, 80) : null, status: record.status, transferred: true };
  if (record.status === "REMOVED") {
    out.lifecycle = str(record.lifecycle, 40, 1);
    if (out.lifecycle === "ACTIVE") fail("ARCHIVE_INVALID");
    return out;
  }
  out.lifecycle = "ACTIVE";
  out.replyToMessageId = record.replyToMessageId ? uuid(record.replyToMessageId, "ARCHIVE_INVALID") : null;
  out.targetMessageId = record.targetMessageId ? uuid(record.targetMessageId, "ARCHIVE_INVALID") : null;
  out.payload = parsePayload(enc.encode(JSON.stringify({ v: 1, ...record.payload })));
  if (record.clientMessageId) out.clientMessageId = uuid(record.clientMessageId, "ARCHIVE_INVALID");
  if (out.payload.t === "file") {
    out.attachmentId = uuid(record.attachmentId, "ARCHIVE_INVALID");
    const info = core.descriptor_info(b64u.dec(out.payload.d));
    try { Object.assign(out, { fileName: info.fileName, mediaType: info.mediaType, size: Number(info.size) }); } finally { info.free(); }
  }
  return out;
}
async function restoreTransfer(state) {
  const secret = b64u.dec(state.secret);
  try { requireDevice().importTransferSecret(secret); } finally { secret.fill(0); }
}
async function historyCheckpoint(state) {
  return requireDevice().signHistoryCheckpoint(state.context, state.descriptor.snapshotId, BigInt(state.descriptor.expiresAt), b64u.dec(state.descriptor.manifestDigest), state.descriptor.chunkCount, BigInt(state.descriptor.totalPlaintextBytes), state.nextIndex);
}

// ------------------------------------------------------------- operations

const ops = {
  /** Opens (or finds no) device of `account` (its principal id) in this browser. */
  async open({ account, scope }) {
    uuid(account);
    for (const k of ["tenantId", "applicationId", "environmentId"]) uuid(scope?.[k]);
    await loadCore();
    if (ctx && ctx.account !== account) fail("ANOTHER_ACCOUNT_OPEN");
    if (ctx && JSON.stringify(ctx.scope) !== JSON.stringify(scope)) fail("LOCAL_BINDING_MISMATCH");
    if (ctx?.device) return ops.status();
    if (ctx?.localProtection && !ctx.key) return { status: "AUTHENTICATOR_REQUIRED", descriptor: ctx.localProtection.descriptor };
    if (lockedAccount !== account) {
      if (!(await acquireLock(account))) return { status: "LOCKED" };
      lockedAccount = account;
    }
    const d = await db();
    let key = await req(d.transaction("keys").objectStore("keys").get(account));
    if (!key) {
      key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
      const tx = d.transaction("keys", "readwrite");
      tx.objectStore("keys").put(key, account);
      await done(tx);
    }
    if (key?.kind === LOCAL_KEY_KIND) {
      const descriptor = localDescriptor(key.descriptor, account, scope);
      ctx = { account, scope: { ...scope }, key: null, localProtection: { ...key, descriptor }, device: null, deviceId: null, persistent: false, meta: { cursors: {}, kt: null }, snapshot: null };
      return { status: "AUTHENTICATOR_REQUIRED", descriptor };
    }
    if (!(key instanceof CryptoKey) || key.extractable) fail("WRAPPING_KEY_EXTRACTABLE");
    ctx = { account, scope: { ...scope }, key, device: null, deviceId: null, persistent: false, meta: { cursors: {}, kt: null }, snapshot: null };
    return restoreDevice();
  },

  async unlock({ prfSecret }) {
    bytes(prfSecret);
    try {
      if (!ctx?.localProtection || ctx.key || ctx.device) fail("LOCAL_UNLOCK_REFUSED");
      const descriptor = localDescriptor(ctx.localProtection.descriptor, ctx.account, ctx.scope);
      const wrapping = await localWrappingKey(descriptor, prfSecret);
      const raw = await unseal(wrapping, localAad(descriptor), ctx.localProtection.wrappedKey);
      try {
        if (raw.length !== 32) fail("LOCAL_UNLOCK_REFUSED");
        ctx.key = await crypto.subtle.importKey("raw", raw, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
      } finally { raw.fill(0); }
      try { return await restoreDevice(); } catch (error) { ctx.key = null; throw error; }
    } finally { prfSecret.fill(0); }
  },

  /** A cancelled/failed migration leaves the original key and records usable. */
  async protectLocal({ descriptor: proposed, prfSecret }) {
    bytes(prfSecret);
    try {
      requireDevice();
      if (!ctx.persistent || !ctx.key) fail("DEVICE_NOT_ADOPTED");
      const descriptor = localDescriptor(proposed, ctx.account, ctx.scope);
      if (descriptor.messagingDeviceId !== ctx.deviceId) fail("LOCAL_BINDING_MISMATCH");
      if (ctx.localProtection) fail("LOCAL_ALREADY_PROTECTED");
      if (ctx.snapshot) fail("LOCAL_MIGRATION_BUSY");
      // Checkpoint the current ratchet before making the ciphertext-only snapshot.
      await persistDevice();
      await discardLocalStages(ctx.account);
      const wrapping = await localWrappingKey(descriptor, prfSecret);
      const raw = crypto.getRandomValues(new Uint8Array(32));
      let newKey, wrappedKey;
      try {
        newKey = await crypto.subtle.importKey("raw", raw, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
        wrappedKey = await seal(wrapping, localAad(descriptor), raw);
      } finally { raw.fill(0); }
      const localProtection = { kind: LOCAL_KEY_KIND, descriptor, wrappedKey };
      const prefix = `${LOCAL_STAGE_PREFIX}${crypto.randomUUID()}:`;
      let ordinal = 0;
      const stage = async (entries) => {
        const tx = (await db()).transaction("pending", "readwrite");
        for (const entry of entries) tx.objectStore("pending").put(entry, [ctx.account, prefix + String(ordinal++).padStart(16, "0")]);
        await done(tx);
      };
      try {
        for (const storeName of ["records", "pending"]) {
          let after = null;
          for (;;) {
            const { keys, values } = await storagePage(storeName, ctx.account, after);
            if (!keys.length) break;
            const staged = [];
            for (let i = 0; i < keys.length; i++) {
              const key = keys[i];
              if (storeName === "pending" && key[1].startsWith(LOCAL_STAGE_PREFIX)) continue;
              const aad = storeName === "records" ? recordAad(ctx.account, key[1], key[2]) : pendingValueAad(ctx.account, key[1]);
              const plain = await unseal(ctx.key, aad, values[i]);
              try { staged.push({ store: storeName, key, value: await seal(newKey, aad, plain) }); } finally { plain.fill(0); }
            }
            if (staged.length) await stage(staged);
            after = keys[keys.length - 1];
          }
        }
        const d = await db();
        const sealedDevice = await req(d.transaction("devices").objectStore("devices").get(ctx.account));
        const plainDevice = await unseal(ctx.key, deviceAad(ctx.account), sealedDevice);
        let newDevice;
        try { newDevice = await seal(newKey, deviceAad(ctx.account), plainDevice); } finally { plainDevice.fill(0); }
        // No asynchronous crypto inside the committing transaction. The cursor copies
        // already sealed pages; replacement of every value and removal of the usable
        // legacy key either all commit, or all abort, including quota failures.
        const tx = d.transaction(["keys", "devices", "records", "pending"], "readwrite");
        const completion = done(tx);
        const cursor = tx.objectStore("pending").openCursor(IDBKeyRange.bound([ctx.account, prefix], [ctx.account, prefix + "\uffff"]));
        cursor.onsuccess = () => {
          const entry = cursor.result;
          if (!entry) return;
          const staged = entry.value;
          tx.objectStore(staged.store).put(staged.value, staged.key);
          entry.delete();
          entry.continue();
        };
        tx.objectStore("devices").put(newDevice, ctx.account);
        tx.objectStore("keys").put(localProtection, ctx.account);
        await completion;
        ctx.key = newKey;
        ctx.localProtection = localProtection;
        return { localProtection: descriptor };
      } catch (error) {
        await discardLocalStages(ctx.account).catch(() => {});
        throw error;
      }
    } finally { prfSecret.fill(0); }
  },

  /** The page also terminates this worker to discard its WebAssembly heap. */
  async lock() {
    ctx?.snapshot?.fill(0);
    ctx?.device?.free?.();
    if (ctx) { ctx.device = null; ctx.key = null; ctx.meta = { cursors: {}, kt: null }; ctx.snapshot = null; }
    handles.clear();
    return true;
  },

  /** New device keys for the server-assigned device id (not stored until `adopt`). */
  async generate({ deviceId }) {
    if (!ctx) fail("NOT_OPEN");
    if (ctx.persistent) fail("DEVICE_EXISTS");
    ctx.device?.free?.();
    ctx.device = new core.WebDevice(uuid(deviceId));
    ctx.meta.provenanceVersion = 1;
    ctx.deviceId = deviceId;
    return { deviceId, signaturePublicKey: ctx.device.signaturePublicKey };
  },

  async registrationKeyPackage({ challengeId, nonce }) {
    if (ctx?.snapshot) fail("MLS_CHANGE_PENDING");
    const keyPackage = requireDevice().registrationKeyPackage(uuid(challengeId), bytes(nonce));
    await persistDevice();
    return keyPackage;
  },

  /** The server accepted the registration: the device becomes durable. */
  async adopt() {
    requireDevice();
    ctx.persistent = true;
    await persistDevice();
    return { deviceId: ctx.deviceId };
  },

  /** Fresh KeyPackages; their private keys are stored BEFORE the page may publish them. */
  async keyPackages({ count }) {
    const device = requireDevice();
    if (ctx.snapshot) fail("MLS_CHANGE_PENDING");
    if (!ctx.persistent) fail("DEVICE_NOT_ADOPTED");
    if (!Number.isInteger(count) || count < 1 || count > 20) fail("BAD_COUNT");
    const out = [];
    for (let i = 0; i < count; i++) out.push(device.keyPackage());
    await persistDevice();
    return out;
  },

  async status() {
    return ctx?.device
      ? { status: ctx.persistent ? "READY" : "PENDING", deviceId: ctx.deviceId, signaturePublicKey: ctx.device.signaturePublicKey, cursors: ctx.meta.cursors, processedCursors: ctx.meta.processed ?? {}, ...(ctx.localProtection ? { localProtection: ctx.localProtection.descriptor } : {}) }
      : { status: ctx ? "NONE" : "CLOSED" };
  },

  async hasGroup({ groupId }) {
    return requireDevice().hasGroup(bytes(groupId));
  },
  async epoch({ groupId }) {
    return Number(requireDevice().epoch(bytes(groupId)));
  },
  async members({ groupId }) {
    return requireDevice().members(bytes(groupId));
  },

  /** A group change the Delivery Service may still refuse: snapshot first. */
  async createGroup({ groupId }) {
    const device = requireDevice();
    ctx.snapshot ??= device.exportState();
    ctx.membershipSnapshot ??= structuredClone(ctx.meta.membership ?? {});
    const epoch = Number(device.createGroup(bytes(groupId)));
    ctx.meta.membership ??= {};
    ctx.meta.membership[b64u.enc(groupId)] = { epoch, firstJoinKnown: ctx.meta.provenanceVersion === 1, method: "CREATE" };
    return epoch;
  },
  async addDevices({ groupId, keyPackages }) {
    const device = requireDevice();
    if (!Array.isArray(keyPackages) || keyPackages.length === 0 || keyPackages.length > 200) fail("BAD_KEY_PACKAGES");
    ctx.snapshot ??= device.exportState();
    let size = 0;
    for (const k of keyPackages) size += 4 + bytes(k).length;
    const packed = new Uint8Array(size);
    const view = new DataView(packed.buffer);
    let at = 0;
    for (const k of keyPackages) {
      view.setUint32(at, k.length);
      packed.set(k, at + 4);
      at += 4 + k.length;
    }
    const r = device.addDevices(bytes(groupId), packed);
    return { commit: r.commit, welcome: r.welcome ?? null, epoch: Number(r.epoch), groupInfo: device.groupInfo(bytes(groupId)) };
  },
  async removeDevices({ groupId, deviceIds }) {
    const device = requireDevice();
    ctx.snapshot ??= device.exportState();
    const r = device.removeDevices(bytes(groupId), deviceIds.map((d) => uuid(d)));
    return { commit: r.commit, epoch: Number(r.epoch), groupInfo: device.groupInfo(bytes(groupId)) };
  },
  /** A commit with a fresh leaf for this device and no membership change (publishes a GroupInfo, attests moot removals). */
  async updateKeys({ groupId }) {
    const device = requireDevice();
    ctx.snapshot ??= device.exportState();
    const r = device.updateKeys(bytes(groupId));
    return { commit: r.commit, epoch: Number(r.epoch), groupInfo: device.groupInfo(bytes(groupId)) };
  },
  /**
   * Joins a group BY ITSELF from its GroupInfo (an MLS external commit): no other member
   * has to be online. Like every group change it is undone (`rollback`) if the Delivery
   * Service refuses the commit.
   */
  async externalJoin({ groupId, groupInfo }) {
    const device = requireDevice();
    ctx.snapshot ??= device.exportState();
    ctx.membershipSnapshot ??= structuredClone(ctx.meta.membership ?? {});
    const r = device.externalJoin(bytes(groupId), bytes(groupInfo));
    ctx.meta.membership ??= {};
    ctx.meta.membership[b64u.enc(groupId)] = { epoch: Number(device.epoch(groupId)), firstJoinKnown: ctx.meta.provenanceVersion === 1, method: "EXTERNAL" };
    return { commit: r.commit, epoch: Number(r.epoch), groupInfo: device.groupInfo(bytes(groupId)) };
  },
  /** The Delivery Service accepted the change: make it durable. */
  async confirm() {
    ctx.snapshot?.fill(0);
    ctx.snapshot = null;
    ctx.membershipSnapshot = null;
    await persistDevice();
    return true;
  },
  /** The Delivery Service refused it: return to the state before the change. */
  async rollback() {
    if (!ctx?.snapshot) return false;
    ctx.device.free?.();
    ctx.device = core.WebDevice.fromState(ctx.deviceId, ctx.snapshot);
    ctx.snapshot.fill(0);
    ctx.snapshot = null;
    if (ctx.membershipSnapshot) ctx.meta.membership = ctx.membershipSnapshot;
    ctx.membershipSnapshot = null;
    return true;
  },

  async join({ groupId, welcome }) {
    const epoch = Number(requireDevice().join(bytes(groupId), bytes(welcome)));
    ctx.meta.membership ??= {};
    ctx.meta.membership[b64u.enc(groupId)] ??= { epoch, firstJoinKnown: ctx.meta.provenanceVersion === 1, method: "WELCOME" };
    await persistDevice();
    return epoch;
  },

  inspectKeyPackage({ keyPackage, now }) {
    const info = core.inspect_key_package(bytes(keyPackage), BigInt(now));
    return { deviceId: info.deviceId, signatureKey: info.signatureKey };
  },

  /**
   * Key transparency (Part V G.5): the device and exact signature key a KeyPackage names
   * must be in the log, under a tree head signed by the pinned log key, not revoked
   * later, and the head must extend every head this device saw before (no rollback).
   */
  async verifyTransparency({ proof, logKey, logId, expectedDeviceId, expectedKey }) {
    requireDevice();
    const head = proof?.treeHead;
    if (!head || typeof head.treeSize !== "number") fail("KT_PROOF_INVALID");
    const root = b64u.dec(head.rootHash);
    if (!logKey) return { status: "UNPINNED" };
    if (head.logId !== logId) fail("KT_LOG_MISMATCH");
    const signed = core.verify_tree_head(b64u.dec(logKey), head.logId, BigInt(head.treeSize), root, BigInt(head.timestampMicros), b64u.dec(head.signature));
    if (!signed) fail("KT_SIGNATURE_INVALID");
    let registered = false;
    let revoked = false;
    for (const e of proof.entries ?? []) {
      const leaf = b64u.dec(e.leafData);
      const path = (e.inclusionProof ?? []).map((h) => b64u.dec(h));
      const flat = new Uint8Array(path.length * 32);
      path.forEach((h, i) => flat.set(h, i * 32));
      if (!core.verify_inclusion(BigInt(e.leafIndex), BigInt(head.treeSize), leaf, flat, root)) fail("KT_INCLUSION_INVALID");
      const doc = JSON.parse(dec.decode(leaf));
      if (doc.deviceId !== expectedDeviceId) fail("KT_DEVICE_MISMATCH");
      if (e.kind === "DEVICE_REGISTERED") {
        if (b64u.enc(b64u.dec(doc.signaturePublicKey)) !== b64u.enc(bytes(expectedKey))) fail("KT_KEY_MISMATCH");
        registered = true;
      } else if (e.kind === "DEVICE_REVOKED") {
        revoked = true;
      }
    }
    if (!registered) fail("KT_NOT_LOGGED");
    if (revoked) fail("KT_DEVICE_REVOKED");
    const seen = ctx.meta.kt;
    if (seen && seen.treeSize > 0 && seen.treeSize !== head.treeSize) {
      const c = proof.consistency;
      if (!c || c.fromTreeSize !== seen.treeSize) fail("KT_CONSISTENCY_REQUIRED");
      const cp = c.proof.map((h) => b64u.dec(h));
      const flat = new Uint8Array(cp.length * 32);
      cp.forEach((h, i) => flat.set(h, i * 32));
      if (b64u.dec(c.fromRootHash).join() !== b64u.dec(seen.rootHash).join()) fail("KT_ROLLBACK_DETECTED");
      if (head.treeSize < seen.treeSize || !core.verify_consistency(BigInt(seen.treeSize), BigInt(head.treeSize), b64u.dec(seen.rootHash), root, flat)) {
        fail("KT_ROLLBACK_DETECTED");
      }
    } else if (seen && seen.treeSize === head.treeSize && seen.rootHash !== head.rootHash) {
      fail("KT_EQUIVOCATION_DETECTED");
    }
    ctx.meta.kt = { treeSize: head.treeSize, rootHash: head.rootHash };
    await persistDevice();
    return { status: "VERIFIED", treeSize: head.treeSize };
  },
  async knownTreeSize() {
    return ctx?.meta?.kt?.treeSize ?? null;
  },

  /** Encrypts one message (text, or a prepared attachment) for the conversation's group. */
  async encrypt({ conversationId, groupId, clientMessageId, payload }) {
    const device = requireDevice();
    if (ctx.snapshot) fail("MLS_CHANGE_PENDING");
    uuid(conversationId);
    uuid(clientMessageId);
    let p;
    let attachmentId = null;
    let fileMeta = {};
    if (payload?.t === "text") {
      if (typeof payload.text !== "string" || payload.text.length < 1 || payload.text.length > MAX_TEXT) fail("TEXT_INVALID");
      p = { v: 1, t: "text", text: payload.text };
      const preview = linkPreview(payload.preview);
      if (preview) p.preview = preview;
    } else if (payload?.t === "file") {
      const h = handles.get(payload.handle) ?? fail("UNKNOWN_ATTACHMENT");
      attachmentId = uuid(payload.attachmentId);
      const bound = h.file.descriptorFor(attachmentId);
      p = { v: 1, t: "file", d: b64u.enc(bound), caption: typeof payload.caption === "string" ? payload.caption.slice(0, 1024) : null };
      fileMeta = { fileName: h.fileName, mediaType: h.mediaType, size: h.size };
      handles.delete(payload.handle);
      h.file.free();
    } else {
      p = richPayload(payload);
    }
    const plain = enc.encode(JSON.stringify(p));
    const ciphertext = device.encrypt(bytes(groupId), aadFor(conversationId, clientMessageId, ctx.deviceId), plain);
    const epoch = Number(device.epoch(bytes(groupId)));
    // What this device sent: it cannot decrypt its own message later (MLS), so the
    // payload is kept (encrypted) until the Delivery Service echoes the message back.
    const own = { payload: p, attachmentId, ...fileMeta };
    const pending = await seal(ctx.key, pendingAad(ctx.account, clientMessageId), enc.encode(JSON.stringify(own)));
    await persistDevice([["pending", [ctx.account, clientMessageId], pending]]);
    return { ciphertext, epoch };
  },

  /**
   * Processes the conversation's messages in sequence order (what the Delivery Service
   * returned after this device's cursor): commits are verified and merged, application
   * messages decrypted under the CF Meet AAD of their envelope. Every outcome, the new
   * group state and the cursor are stored in ONE IndexedDB transaction.
   */
  async processMessages({ conversationId, groupId, messages, allowedJoiners = [], rejectedJoiners = [], visibleFromSequence }) {
    let device = requireDevice();
    if (ctx.snapshot) fail("MLS_CHANGE_PENDING");
    uuid(conversationId);
    const gid = bytes(groupId);
    const cursor = ctx.meta.cursors[conversationId] ?? 0;
    const newlyFailed = new Set();
    const resolved = new Set();
    const visibleFrom = visibleFromSequence ?? ctx.meta.visibleFrom?.[conversationId] ?? 1;
    if (!Number.isSafeInteger(visibleFrom) || visibleFrom < 1) fail("HISTORY_VISIBILITY_REQUIRED");
    const savedState = device.exportState();
    const savedMeta = structuredClone(ctx.meta);
    const pendingCommit = ctx.meta.pendingCommit?.[conversationId] >= visibleFrom ? ctx.meta.pendingCommit[conversationId] : null;
    let processed = ctx.meta.processed?.[conversationId] ?? cursor;
    const membership = ctx.meta.membership?.[b64u.enc(gid)];
    const sorted = [...messages].sort((a, b) => a.sequence - b.sequence);
    const allowed = Array.isArray(allowedJoiners) ? allowedJoiners.filter((a) => typeof a === "string") : [];
    const rejected = new Set(Array.isArray(rejectedJoiners) ? rejectedJoiners : []);
    const out = [];
    const puts = [];
    const deletes = [];
    let last = Math.max(cursor, visibleFrom - 1);
    let joiner = null;
    let blocked = null;
    try {
    ctx.meta.visibleFrom ??= {};
    ctx.meta.visibleFrom[conversationId] = visibleFrom;
    if (ctx.meta.pendingCommit?.[conversationId] < visibleFrom) delete ctx.meta.pendingCommit[conversationId];
    if (visibleFrom > 1) {
      deletes.push(["pending", IDBKeyRange.bound([ctx.account, retryKey(conversationId, 0)], [ctx.account, retryKey(conversationId, visibleFrom)], false, true)]);
    }
    for (const m of sorted) {
      if (!Number.isSafeInteger(m.sequence) || m.sequence < 1) fail("BAD_SEQUENCE");
      if (m.sequence < visibleFrom) fail("HISTORY_VISIBILITY_CHANGED");
      const reference = await req((await db()).transaction("pending").objectStore("pending").getKey([ctx.account, retryKey(conversationId, m.sequence)]));
      if (m.sequence <= last && reference === undefined && (!m.lifecycle || m.lifecycle === "ACTIVE")) continue;
      // The authoritative endpoint returns every visible row, including tombstones.
      // Initial visibility may begin after 1; thereafter never cross a hole/commit.
      if (last > 0 && m.sequence > last + 1) { blocked = "SYNC_GAP"; break; }
      if (pendingCommit && pendingCommit < m.sequence && !resolved.has(pendingCommit)) { blocked = "COMMIT_PENDING"; break; }
      const existing = await loadRecord(conversationId, m.sequence);
      if (existing && ["DECRYPTED", "SENT", "REMOVED", "COMMIT"].includes(existing.status)) {
        if (m.lifecycle && m.lifecycle !== "ACTIVE") {
          const tombstone = { sequence: m.sequence, messageId: m.messageId, senderPrincipalId: m.senderPrincipalId, senderDeviceId: m.senderDeviceId, createdAt: m.createdAt, kind: m.kind, status: "REMOVED", lifecycle: m.lifecycle };
          puts.push(["records", [ctx.account, conversationId, m.sequence], await seal(ctx.key, recordAad(ctx.account, conversationId, m.sequence), enc.encode(JSON.stringify(tombstone)))]);
          out.push(view(tombstone));
        }
        resolved.add(m.sequence);
        if (ctx.meta.pendingCommit?.[conversationId] === m.sequence) delete ctx.meta.pendingCommit[conversationId];
        deletes.push(["pending", [ctx.account, retryKey(conversationId, m.sequence)]]);
        last = Math.max(last, m.sequence); continue;
      }
      const base = { sequence: m.sequence, messageId: m.messageId, senderPrincipalId: m.senderPrincipalId, senderDeviceId: m.senderDeviceId, createdAt: m.createdAt, kind: m.kind, replyToMessageId: m.replyToMessageId ?? null, targetMessageId: m.targetMessageId ?? null };
      let record;
      // MLS can consume a sender generation before application-level AAD/payload
      // validation rejects it. A failed attempt must leave that generation retryable.
      const beforeMessage = device.exportState();
      try {
        if (m.lifecycle && m.lifecycle !== "ACTIVE") {
          record = { ...base, status: "REMOVED", lifecycle: m.lifecycle };
        } else if (m.kind === "MLS_COMMIT") {
          const own = m.senderDeviceId === ctx.deviceId;
          const epoch = Number(device.epoch(gid));
          if (own || m.epoch < epoch) {
            record = { ...base, status: "COMMIT", epoch };
          } else {
            // An external commit (a member's device joining by itself) is merged only for
            // the device that sent it, verified by the page (registered, ACTIVE, of a
            // member, with exactly its logged key).
            const sender = typeof m.senderDeviceId === "string" ? m.senderDeviceId : "";
            const r = device.process(gid, b64u.dec(m.ciphertext), undefined, allowed.filter((a) => a.startsWith(`${sender}:`)));
            if (r.kind === "JOINER_UNVERIFIED") {
              if (!rejected.has(r.joiner) && r.joiner === sender) {
                // Stop before it: the page verifies the joiner and processes it again.
                joiner = { deviceId: r.joiner, sequence: m.sequence, senderPrincipalId: m.senderPrincipalId };
                ctx.device.free();
                ctx.device = core.WebDevice.fromState(ctx.deviceId, beforeMessage);
                device = ctx.device;
                break;
              }
              fail("JOINER_REJECTED");
            }
            if (r.kind !== "COMMIT") fail("UNEXPECTED_MESSAGE");
            record = { ...base, status: "COMMIT", epoch: Number(r.epoch), ...(r.joiner ? { joiner: r.joiner } : {}) };
          }
        } else if (m.senderDeviceId === ctx.deviceId) {
          const sealed = await req((await db()).transaction("pending").objectStore("pending").get([ctx.account, m.clientMessageId]));
          if (sealed) {
            const own = JSON.parse(dec.decode(await unseal(ctx.key, pendingAad(ctx.account, m.clientMessageId), sealed)));
            record = { ...base, status: "SENT", ...own };
            deletes.push(["pending", [ctx.account, m.clientMessageId]]);
          } else {
            record = { ...base, status: "SENT_ELSEWHERE" };
          }
        } else if (membership?.firstJoinKnown && m.epoch < membership.epoch) {
          record = { ...base, status: "BEFORE_JOIN", beforeJoinVerified: true };
        } else {
          const r = device.process(gid, b64u.dec(m.ciphertext), aadFor(conversationId, uuid(m.clientMessageId), uuid(m.senderDeviceId)), []);
          if (r.kind !== "APPLICATION") fail("UNEXPECTED_MESSAGE");
          let payload;
          try { payload = parsePayload(r.plaintext); } finally { r.plaintext.fill(0); }
          record = { ...base, status: "DECRYPTED", payload, attachmentId: (m.attachmentIds ?? [])[0] ?? null };
          if (payload.t === "file") {
            // Presentation metadata comes from the descriptor (protected), never the server.
            const info = core.descriptor_info(b64u.dec(payload.d));
            Object.assign(record, { fileName: info.fileName, mediaType: info.mediaType, size: Number(info.size) });
            info.free();
          }
        }
      } catch (e) {
        const historical = m.epoch < Number(device.epoch(gid));
        ctx.device.free();
        ctx.device = core.WebDevice.fromState(ctx.deviceId, beforeMessage);
        device = ctx.device;
        const code = e instanceof Error && /^[A-Z][A-Z0-9_]{2,63}$/.test(e.message) ? e.message : "FAILED";
        record = { ...base, status: "FAILED", error: historical && code === "MESSAGE_REFUSED" ? "HISTORICAL_EPOCH_UNAVAILABLE" : code };
      } finally { beforeMessage.fill(0); }
      if (record.status === "FAILED") {
        newlyFailed.add(m.sequence);
        puts.push(await retryPut(conversationId, m.sequence));
      } else {
        resolved.add(m.sequence);
        deletes.push(["pending", [ctx.account, retryKey(conversationId, m.sequence)]]);
        if (m.kind === "MLS_COMMIT" && ctx.meta.pendingCommit?.[conversationId] === m.sequence) delete ctx.meta.pendingCommit[conversationId];
      }
      // A failed commit must be retried before processing the next epoch.
      if (record.status === "FAILED" && m.kind === "MLS_COMMIT") {
        ctx.meta.pendingCommit ??= {};
        ctx.meta.pendingCommit[conversationId] = m.sequence;
        blocked = "COMMIT_PENDING";
      }
      last = Math.max(last, m.sequence);
      puts.push(["records", [ctx.account, conversationId, m.sequence], await seal(ctx.key, recordAad(ctx.account, conversationId, m.sequence), enc.encode(JSON.stringify(record)))]);
      out.push(view(record));
      if (blocked) break;
    }
    ctx.meta.cursors[conversationId] = last;
    ctx.meta.processed ??= {};
    const earliest = await firstRetry(conversationId, visibleFrom - 1, resolved);
    const failures = [...newlyFailed, ...(earliest === null ? [] : [earliest])];
    processed = failures.length ? Math.min(last, ...failures) - 1 : last;
    // Until migration has scanned all legacy outcomes, never overclaim progress.
    if (!ctx.meta.retryMigrated?.[conversationId]) processed = Math.min(processed, ctx.meta.processed[conversationId] ?? 0);
    ctx.meta.processed[conversationId] = processed;
    await persistDevice(puts, deletes);
    return { cursor: last, processedCursor: processed, records: out, joiner, blocked };
    } catch (e) {
      // IndexedDB aborts must not leave in-memory ratchets ahead of durable state.
      ctx.device.free();
      ctx.device = core.WebDevice.fromState(ctx.deviceId, savedState);
      ctx.meta = savedMeta;
      throw e;
    } finally { savedState.fill(0); }
  },

  async retryCursor({ conversationId }) {
    uuid(conversationId);
    if (!ctx) return null;
    const previous = structuredClone(ctx.meta);
    try {
    // Incrementally migrate old failures in bounded pages. References are stored
    // atomically with scan progress; all legacy records remain intact.
    ctx.meta.retryMigrated ??= {};
    ctx.meta.retryScan ??= {};
    if (!ctx.meta.retryMigrated[conversationId]) {
      const after = Math.max(ctx.meta.retryScan[conversationId] ?? 0, (ctx.meta.visibleFrom?.[conversationId] ?? 1) - 1);
      const d = await db();
      const range = IDBKeyRange.bound([ctx.account, conversationId, after], [ctx.account, conversationId, Number.MAX_SAFE_INTEGER], true);
      const keys = await req(d.transaction("records").objectStore("records").getAllKeys(range, 128));
      const puts = [];
      for (const key of keys) {
        const r = await loadRecord(conversationId, key[2]);
        if (r?.status === "FAILED" || (r?.status === "BEFORE_JOIN" && !r.beforeJoinVerified)) {
          puts.push(await retryPut(conversationId, key[2]));
          if (r.kind === "MLS_COMMIT") {
            ctx.meta.pendingCommit ??= {};
            ctx.meta.pendingCommit[conversationId] = Math.min(ctx.meta.pendingCommit[conversationId] ?? key[2], key[2]);
          }
        }
      }
      if (keys.length) ctx.meta.retryScan[conversationId] = keys[keys.length - 1][2];
      if (keys.length < 128) ctx.meta.retryMigrated[conversationId] = true;
      await persistDevice(puts);
    }
    const firstVisible = ctx.meta.visibleFrom?.[conversationId] ?? 1;
    if (ctx.meta.pendingCommit?.[conversationId] >= firstVisible) return ctx.meta.pendingCommit[conversationId] - 1;
    ctx.meta.retryTurn ??= {};
    const turn = Math.max(ctx.meta.retryTurn[conversationId] ?? 0, firstVisible - 1);
    const next = await firstRetry(conversationId, turn) ?? await firstRetry(conversationId, firstVisible - 1);
    if (next === null) return null;
    ctx.meta.retryTurn[conversationId] = next;
    await persistDevice();
    return next - 1;
    } catch (e) { ctx.meta = previous; throw e; }
  },

  async cursor({ conversationId }) {
    return ctx?.meta?.cursors?.[uuid(conversationId)] ?? 0;
  },

  /** Stored history of a conversation, newest last (at most `limit`). */
  async exportLocalHistory({ account, after = null }) {
    // Read-only: no device registration, key creation, MLS state export or network call.
    uuid(account);
    const d = await db();
    const storedKey = await req(d.transaction("keys").objectStore("keys").get(account));
    if (!storedKey) return { records: [], next: null };
    const key = storedKey?.kind === LOCAL_KEY_KIND ? (ctx?.account === account && ctx?.device && ctx.key ? ctx.key : fail("LOCAL_UNLOCK_REQUIRED")) : storedKey;
    let lower = [account, "", 0];
    if (after) {
      uuid(after.conversationId);
      if (!Number.isSafeInteger(after.sequence) || after.sequence < 0) fail("BAD_CURSOR");
      lower = [account, after.conversationId, after.sequence];
    }
    const range = IDBKeyRange.bound(lower, [account, "\uffff", Number.MAX_SAFE_INTEGER], !!after);
    const store = d.transaction("records").objectStore("records");
    const keysPromise = req(store.getAllKeys(range, 101));
    const valuesPromise = req(store.getAll(range, 101));
    const [keys, values] = await Promise.all([keysPromise, valuesPromise]);
    const records = [];
    for (let i = 0; i < Math.min(values.length, 100); i++) {
      const [, conversationId, sequence] = keys[i];
      if (ctx?.account === account && sequence < (ctx.meta.visibleFrom?.[conversationId] ?? 1)) continue;
      const record = JSON.parse(dec.decode(await unseal(key, recordAad(account, conversationId, sequence), values[i])));
      records.push(exportRecord(conversationId, await redactRemovedRecord(conversationId, record, null, account, key)));
    }
    const last = keys[Math.min(keys.length, 100) - 1];
    return { records, next: keys.length > 100 ? { conversationId: last[1], sequence: last[2] } : null };
  },

  /** Stored history of a conversation, newest last (at most `limit`). */
  async history({ conversationId, limit = 200 }) {
    requireDevice();
    uuid(conversationId);
    const d = await db();
    const range = IDBKeyRange.bound([ctx.account, conversationId, ctx.meta.visibleFrom?.[conversationId] ?? 1], [ctx.account, conversationId, Number.MAX_SAFE_INTEGER]);
    const store = d.transaction("records").objectStore("records");
    const keys = await req(store.getAllKeys(range));
    const values = await req(store.getAll(range));
    const out = [];
    for (let i = Math.max(0, values.length - limit); i < values.length; i++) {
      const seq = keys[i][2];
      const plain = await unseal(ctx.key, recordAad(ctx.account, conversationId, seq), values[i]);
      try { out.push(view(await redactRemovedRecord(conversationId, JSON.parse(dec.decode(plain))))); } finally { plain.fill(0); }
    }
    return out;
  },

  /** Scan durable outcomes, including EDIT/control records hidden by the timeline. */
  async historyRecoveryNeeded() {
    requireDevice();
    const { keys, values } = await storagePage("records", ctx.account, ctx.meta.historyRecoveryScan ?? null);
    let needed = false;
    for (let i = 0; i < keys.length; i++) {
      const [, conversationId, sequence] = keys[i];
      if (sequence < (ctx.meta.visibleFrom?.[conversationId] ?? 1)) continue;
      const plain = await unseal(ctx.key, recordAad(ctx.account, conversationId, sequence), values[i]);
      let record;
      try { record = JSON.parse(dec.decode(plain)); } finally { plain.fill(0); }
      record = await redactRemovedRecord(conversationId, record);
      if (record.kind !== "MLS_COMMIT" && ["FAILED", "BEFORE_JOIN", "SENT_ELSEWHERE"].includes(record.status)) needed = true;
    }
    ctx.meta.historyRecoveryScan = keys.length === 128 ? keys[keys.length - 1] : null;
    await persistDevice();
    return needed;
  },

  /** Encrypts a file for upload; its content key stays here (a handle is returned). */
  async encryptFile({ data, mediaType, fileName }) {
    requireDevice();
    const f = core.encrypt_file(bytes(data), String(mediaType).slice(0, 127), String(fileName).slice(0, 255));
    const handle = crypto.randomUUID();
    // The descriptor (content key) stays in the wasm object until the message is sent.
    handles.set(handle, { file: f, fileName: String(fileName).slice(0, 255), mediaType: String(mediaType).slice(0, 127), size: data.length });
    return { handle, ciphertext: f.ciphertext, sha256: f.sha256 };
  },

  /** Decrypts a downloaded attachment of a stored message (its key never left here). */
  async decryptAttachment({ conversationId, sequence, ciphertext }) {
    requireDevice();
    if (sequence < (ctx.meta.visibleFrom?.[conversationId] ?? 1)) fail("HISTORY_VISIBILITY_CHANGED");
    const r = await loadRecord(uuid(conversationId), sequence);
    if (!r || r.payload?.t !== "file" || !r.attachmentId) fail("NO_SUCH_ATTACHMENT");
    const f = core.decrypt_file(b64u.dec(r.payload.d), r.attachmentId, bytes(ciphertext));
    return { data: f.plaintext, mediaType: f.mediaType, fileName: f.fileName };
  },

  // --------------------------------------------------- history transfer (AD-07)

  async reconcileHistoryMetadata({ conversationId, messages }) {
    requireDevice(); uuid(conversationId);
    if (!Array.isArray(messages) || messages.length > 500) fail("BAD_MESSAGES");
    const puts = [], removals = new Map();
    // Validate and stage references first, including targets with no local record.
    // At most 500 entries live in memory; encrypted references survive page/restart boundaries.
    for (const message of messages) {
      if (!Number.isSafeInteger(message.sequence) || message.sequence < 1) fail("BAD_SEQUENCE");
      uuid(message.messageId);
      const lifecycle = str(message.lifecycle, 40, 1);
      if (lifecycle !== "ACTIVE") {
        removals.set(message.messageId, lifecycle);
        puts.push(await jsonPut(removalKey(conversationId, message.messageId), { lifecycle }));
      }
    }
    for (const message of messages) {
      const existing = await loadRecord(conversationId, message.sequence);
      if (!existing) continue;
      if (existing.messageId !== message.messageId) fail("HISTORY_RECORD_CONFLICT");
      const target = message.targetMessageId ? uuid(message.targetMessageId) : existing.targetMessageId ?? null;
      if (existing.targetMessageId && existing.targetMessageId !== target) fail("HISTORY_RECORD_CONFLICT");
      let updated = { ...existing, lifecycle: message.lifecycle, replyToMessageId: message.replyToMessageId ? uuid(message.replyToMessageId) : null, targetMessageId: target };
      updated = await redactRemovedRecord(conversationId, updated, removals);
      const plain = enc.encode(JSON.stringify(updated));
      try { puts.push(["records", [ctx.account, conversationId, message.sequence], await seal(ctx.key, recordAad(ctx.account, conversationId, message.sequence), plain)]); } finally { plain.fill(0); }
    }
    await persistDevice(puts);
    return true;
  },

  async beginTransferV2({ context, expiresAt }) {
    const device = requireDevice();
    context = historyContext(context, "target"); historyExpiry(expiresAt);
    const key = historyStateKey(context[0], "target");
    const existing = await pendingJson(key);
    if (existing) {
      if (JSON.stringify(existing.context) !== JSON.stringify(context) || existing.expiresAt !== expiresAt) fail("TRANSFER_CONTEXT_INVALID");
      return b64u.dec(existing.publicKey);
    }
    const publicKey = device.beginTransfer();
    const secret = device.exportTransferSecret();
    try {
      const state = { v: 2, role: "target", context, expiresAt, publicKey: b64u.enc(publicKey), secret: b64u.enc(secret), nextIndex: 0, records: 0, imported: 0 };
      await persistDevice([await jsonPut(key, state)]);
      return publicKey;
    } finally { secret.fill(0); device.finishTransfer(); }
  },
  async signTransferRequestV2({ context, expiresAt }) {
    context = historyContext(context, "target"); historyExpiry(expiresAt);
    const state = await pendingJson(historyStateKey(context[0], "target"));
    if (!state || state.expiresAt !== expiresAt || JSON.stringify(state.context) !== JSON.stringify(context)) fail("NO_TRANSFER");
    await restoreTransfer(state);
    try { return requireDevice().signTransferRequestV2(context, BigInt(expiresAt)); } finally { requireDevice().finishTransfer(); }
  },
  verifyTransferRequestV2({ targetKey, context, transferKey, expiresAt, signature }) {
    context = historyContext(context, "source"); historyExpiry(expiresAt);
    return core.verifyTransferRequestV2(bytes(targetKey), context, bytes(transferKey), BigInt(expiresAt), bytes(signature));
  },

  /** Freeze bounded encrypted pages before publishing an immutable manifest. */
  async prepareHistoryV2({ context, transferKey, expiresAt, eligibleConversations }) {
    requireDevice();
    context = historyContext(context, "source"); historyExpiry(expiresAt);
    if (bytes(transferKey).length !== 32) fail("TRANSFER_CONTEXT_INVALID");
    const visibility = visibilityMap(eligibleConversations);
    const key = historyStateKey(context[0], "source");
    const existing = await pendingJson(key);
    if (existing) {
      if (JSON.stringify(existing.context) !== JSON.stringify(context) || existing.transferKey !== b64u.enc(transferKey) || (existing.descriptor?.expiresAt ?? existing.expiresAt) !== expiresAt) fail("TRANSFER_CONTEXT_INVALID");
      if (existing.descriptor) return { descriptor: existing.descriptor, coverage: existing.coverage, records: existing.records };
      // A crash before manifest publication may restart snapshot generation. Its
      // preparing checkpoint owns every orphan page and supplies their finite TTL.
      await ops.forgetHistoryTransferV2({ transferId: context[0] });
    }
    await persistDevice([await jsonPut(key, { v: 2, role: "source", context, expiresAt, transferKey: b64u.enc(transferKey), preparing: true })]);
    const snapshotId = crypto.randomUUID();
    const chunks = [], coverage = new Map();
    let chunkRecords = [], chunkSize = enc.encode('{"v":2,"records":[]}').length, total = 0, recordCount = 0, readableCount = 0;
    const flush = async () => {
      if (!chunkRecords.length) return;
      if (chunks.length >= HISTORY_MAX_CHUNKS) fail("HISTORY_QUOTA_EXCEEDED");
      const plain = enc.encode(JSON.stringify({ v: 2, records: chunkRecords }));
      try {
        if (plain.length > HISTORY_CHUNK_BYTES || total + plain.length > HISTORY_MAX_BYTES) fail("HISTORY_QUOTA_EXCEEDED");
        chunks.push({ digest: b64u.enc(new Uint8Array(await crypto.subtle.digest("SHA-256", plain))), bytes: plain.length, records: chunkRecords.length });
        total += plain.length;
        await persistDevice([await jsonPut(historyObjectKey(context[0], chunks.length - 1), { snapshotId, plaintext: b64u.enc(plain) })]);
      } finally { plain.fill(0); }
      chunkRecords = []; chunkSize = enc.encode('{"v":2,"records":[]}').length;
    };
    let after = null;
    for (;;) {
      const { keys, values } = await storagePage("records", ctx.account, after);
      if (!keys.length) break;
      for (let i = 0; i < keys.length; i++) {
        const [, conversationId, sequence] = keys[i];
        const window = visibility.get(conversationId);
        if (!window || sequence < window.firstSequence || sequence > window.lastSequence) continue;
        const plain = await unseal(ctx.key, recordAad(ctx.account, conversationId, sequence), values[i]);
        let record;
        try { record = JSON.parse(dec.decode(plain)); } finally { plain.fill(0); }
        record = await redactRemovedRecord(conversationId, record);
        const c = coverage.get(conversationId) ?? { conversationId, firstSequence: window.firstSequence, lastSequence: window.lastSequence, availableRecords: 0, unreadableRecords: 0, missingRecords: 0, observedThrough: window.firstSequence - 1 };
        if (sequence > c.observedThrough + 1) c.missingRecords += sequence - c.observedThrough - 1;
        c.observedThrough = Math.max(c.observedThrough, sequence);
        coverage.set(conversationId, c);
        if (!["SENT", "DECRYPTED", "REMOVED"].includes(record.status)) {
          if (!["COMMIT"].includes(record.status)) c.unreadableRecords++;
          continue;
        }
        const entry = { conversationId, record: historyRecord(record) };
        const size = enc.encode(JSON.stringify(entry)).length + (chunkRecords.length ? 1 : 0);
        if (chunkSize + size > HISTORY_CHUNK_BYTES) await flush();
        chunkRecords.push(entry); chunkSize += size; recordCount++; c.availableRecords++;
        if (record.status !== "REMOVED") readableCount++;
      }
      after = keys[keys.length - 1];
    }
    await flush();
    if (!readableCount) fail("HISTORY_SOURCE_EMPTY");
    for (const window of visibility.values()) if (!coverage.has(window.conversationId)) coverage.set(window.conversationId, { ...window, availableRecords: 0, unreadableRecords: 0, missingRecords: Math.max(0, window.lastSequence - window.firstSequence + 1), observedThrough: window.firstSequence - 1 });
    const summaries = [...coverage.values()].map((c) => ({ ...c, completeThroughSnapshot: c.unreadableRecords === 0 && c.missingRecords === 0 && c.observedThrough >= c.lastSequence }));
    const manifest = { v: 2, snapshotId, context, expiresAt, chunks, totalPlaintextBytes: total, records: recordCount,
      coverage: summaries, sourceCoverage: summaries.every((c) => c.completeThroughSnapshot) ? "SOURCE_AVAILABLE" : "PARTIAL" };
    const plainManifest = enc.encode(JSON.stringify(manifest));
    try {
      if (plainManifest.length > HISTORY_CHUNK_BYTES) fail("HISTORY_QUOTA_EXCEEDED");
      const descriptor = { snapshotId, expiresAt, manifestDigest: b64u.enc(new Uint8Array(await crypto.subtle.digest("SHA-256", plainManifest))), chunkCount: chunks.length, totalPlaintextBytes: total };
      const state = { v: 2, role: "source", context, descriptor, transferKey: b64u.enc(transferKey), coverage: manifest.sourceCoverage, records: recordCount };
      await persistDevice([await jsonPut(key, state), await jsonPut(historyObjectKey(context[0], HISTORY_MANIFEST_INDEX), { snapshotId, plaintext: b64u.enc(plainManifest) })]);
      return { descriptor, coverage: state.coverage, records: recordCount };
    } finally { plainManifest.fill(0); }
  },

  /** Ciphertext is cached before upload, making repeated PUTs byte-identical. */
  async historyObjectV2({ transferId, index }) {
    const state = await pendingJson(historyStateKey(transferId, "source"));
    if (!state) fail("NO_TRANSFER");
    historyContext(state.context, "source"); historyDescriptor(state.descriptor);
    if (!Number.isInteger(index) || (index !== HISTORY_MANIFEST_INDEX && (index < 0 || index >= state.descriptor.chunkCount))) fail("ARCHIVE_INVALID");
    const key = historyObjectKey(transferId, index);
    const object = await pendingJson(key);
    if (!object || object.snapshotId !== state.descriptor.snapshotId) fail("HISTORY_SNAPSHOT_UNAVAILABLE");
    if (object.sealed) return b64u.dec(object.sealed);
    const plain = b64u.dec(object.plaintext);
    try {
      const sealed = requireDevice().sealHistoryObject(...historyObjectArgs(state, index), b64u.dec(state.transferKey), plain);
      await persistDevice([await jsonPut(key, { snapshotId: object.snapshotId, sealed: b64u.enc(sealed) })]);
      return sealed;
    } finally { plain.fill(0); }
  },

  async acceptHistoryManifestV2({ context, descriptor: proposed, sourceKey, sealed }) {
    context = historyContext(context, "target");
    const descriptor = historyDescriptor(proposed);
    const key = historyStateKey(context[0], "target");
    const state = await pendingJson(key);
    if (!state || JSON.stringify(state.context) !== JSON.stringify(context) || state.expiresAt !== descriptor.expiresAt) fail("TRANSFER_CONTEXT_INVALID");
    if (state.descriptor && JSON.stringify(state.descriptor) !== JSON.stringify(descriptor)) fail("TRANSFER_MANIFEST_CHANGED");
    const attempt = { ...state, descriptor };
    await restoreTransfer(state);
    let plain;
    try { plain = requireDevice().openHistoryObject(...historyObjectArgs(attempt, HISTORY_MANIFEST_INDEX), bytes(sourceKey), bytes(sealed)); }
    finally { requireDevice().finishTransfer(); }
    try {
      if (plain.length > HISTORY_CHUNK_BYTES || b64u.enc(new Uint8Array(await crypto.subtle.digest("SHA-256", plain))) !== descriptor.manifestDigest) fail("TRANSFER_MANIFEST_CHANGED");
      const manifest = JSON.parse(dec.decode(plain));
      if (manifest?.v !== 2 || manifest.snapshotId !== descriptor.snapshotId || manifest.expiresAt !== descriptor.expiresAt || JSON.stringify(manifest.context) !== JSON.stringify(context) ||
        manifest.totalPlaintextBytes !== descriptor.totalPlaintextBytes || !Array.isArray(manifest.chunks) || manifest.chunks.length !== descriptor.chunkCount ||
        !Number.isSafeInteger(manifest.records) || manifest.records < 1 || !Array.isArray(manifest.coverage) || !["SOURCE_AVAILABLE", "PARTIAL"].includes(manifest.sourceCoverage)) fail("ARCHIVE_INVALID");
      let total = 0, records = 0;
      for (const chunk of manifest.chunks) {
        if (b64u.dec(chunk.digest).length !== 32 || !Number.isSafeInteger(chunk.bytes) || chunk.bytes < 1 || chunk.bytes > HISTORY_CHUNK_BYTES || !Number.isSafeInteger(chunk.records) || chunk.records < 1) fail("ARCHIVE_INVALID");
        total += chunk.bytes; records += chunk.records;
      }
      if (total !== descriptor.totalPlaintextBytes || records !== manifest.records) fail("ARCHIVE_INVALID");
      if (!state.manifest) {
        attempt.manifest = manifest; attempt.sourceKey = b64u.enc(sourceKey);
        await persistDevice([await jsonPut(key, attempt)]);
      } else if (state.sourceKey !== b64u.enc(sourceKey)) fail("TRANSFER_CONTEXT_INVALID");
      return { nextIndex: state.nextIndex, records: state.records, coverage: manifest.sourceCoverage, descriptor };
    } finally { plain.fill(0); }
  },

  /** Import and checkpoint are durable together, independent of network receipts. */
  async importHistoryChunkV2({ transferId, index, sealed, eligibleConversations }) {
    const key = historyStateKey(transferId, "target");
    const state = await pendingJson(key);
    if (!state?.manifest) fail("NO_TRANSFER");
    historyContext(state.context, "target"); historyDescriptor(state.descriptor);
    if (!Number.isInteger(index) || index < 0 || index >= state.descriptor.chunkCount) fail("ARCHIVE_INVALID");
    if (index > state.nextIndex) fail("HISTORY_CHUNK_GAP");
    if (index < state.nextIndex) return { nextIndex: state.nextIndex, records: state.records, imported: state.imported, signature: await historyCheckpoint(state), coverage: state.manifest.sourceCoverage };
    const visibility = visibilityMap(eligibleConversations);
    await restoreTransfer(state);
    let plain;
    try { plain = requireDevice().openHistoryObject(...historyObjectArgs(state, index), b64u.dec(state.sourceKey), bytes(sealed)); }
    finally { requireDevice().finishTransfer(); }
    try {
      const expected = state.manifest.chunks[index];
      if (plain.length !== expected.bytes || b64u.enc(new Uint8Array(await crypto.subtle.digest("SHA-256", plain))) !== expected.digest) fail("HISTORY_CHUNK_DIGEST_MISMATCH");
      const chunk = JSON.parse(dec.decode(plain));
      if (chunk?.v !== 2 || !Array.isArray(chunk.records) || chunk.records.length !== expected.records) fail("ARCHIVE_INVALID");
      const puts = [], deletes = [], seen = new Set();
      let lastRecord = state.lastRecord ?? null;
      let imported = 0;
      for (const entry of chunk.records) {
        const conversationId = uuid(entry?.conversationId, "ARCHIVE_INVALID");
        const record = await redactRemovedRecord(conversationId, historyRecord(entry.record));
        if (!eligibleRecord(conversationId, record, visibility)) fail("HISTORY_VISIBILITY_CHANGED");
        const identity = `${conversationId}:${record.sequence}`;
        if (seen.has(identity)) fail("ARCHIVE_INVALID"); seen.add(identity);
        if (lastRecord && (conversationId < lastRecord.conversationId || (conversationId === lastRecord.conversationId && record.sequence <= lastRecord.sequence))) fail("ARCHIVE_INVALID");
        lastRecord = { conversationId, sequence: record.sequence };
        const existing = await loadRecord(conversationId, record.sequence);
        if (existing?.messageId && existing.messageId !== record.messageId) fail("HISTORY_RECORD_CONFLICT");
        if (!existing || (!["SENT", "DECRYPTED", "REMOVED"].includes(existing.status)) || (record.status === "REMOVED" && existing.status !== "REMOVED")) {
          const value = enc.encode(JSON.stringify(record));
          try { puts.push(["records", [ctx.account, conversationId, record.sequence], await seal(ctx.key, recordAad(ctx.account, conversationId, record.sequence), value)]); } finally { value.fill(0); }
          deletes.push(["pending", [ctx.account, retryKey(conversationId, record.sequence)]]); imported++;
        }
      }
      const updated = { ...state, nextIndex: index + 1, records: state.records + chunk.records.length, imported: state.imported + imported, lastRecord };
      puts.push(await jsonPut(key, updated));
      await persistDevice(puts, deletes);
      return { nextIndex: updated.nextIndex, records: updated.records, imported: updated.imported, signature: await historyCheckpoint(updated), coverage: state.manifest.sourceCoverage };
    } finally { plain.fill(0); }
  },

  async historyProgressV2({ transferId }) {
    const state = await pendingJson(historyStateKey(transferId, "target"));
    if (!state) fail("NO_TRANSFER");
    historyContext(state.context, "target"); historyExpiry(state.expiresAt);
    return { context: state.context, expiresAt: state.expiresAt, nextIndex: state.nextIndex, records: state.records, imported: state.imported, descriptor: state.descriptor ?? null,
      coverage: state.manifest?.sourceCoverage ?? null, signature: state.descriptor ? await historyCheckpoint(state) : null };
  },
  async listHistoryTransfersV2() {
    requireDevice();
    const out = [];
    let after = null;
    for (;;) {
      const { keys } = await storagePage("pending", ctx.account, after);
      if (!keys.length) break;
      for (const [, key] of keys) if (/^history-v2:[0-9a-f-]{36}:(source|target)$/.test(key)) {
        const state = await pendingJson(key);
        const expiry = state.expiresAt ?? state.descriptor?.expiresAt;
        if (expiry > Math.floor(Date.now() / 1000)) out.push({ transferId: state.context[0], context: state.context, role: state.role, expiresAt: expiry, nextIndex: state.nextIndex ?? 0 });
        else await ops.forgetHistoryTransferV2({ transferId: state.context[0] });
      }
      after = keys[keys.length - 1];
    }
    return out;
  },
  async forgetHistoryTransferV2({ transferId }) {
    requireDevice();
    const prefix = `history-v2:${uuid(transferId)}:`;
    const tx = (await db()).transaction("pending", "readwrite");
    tx.objectStore("pending").delete(IDBKeyRange.bound([ctx.account, prefix], [ctx.account, prefix + "\uffff"]));
    await done(tx);
    requireDevice().finishTransfer();
    return true;
  },

  async beginTransfer() {
    return requireDevice().beginTransfer();
  },
  async signTransferRequest({ context, expiresAt }) {
    return requireDevice().signTransferRequest(context.map((c) => uuid(c)), BigInt(expiresAt));
  },
  verifyTransferRequest({ targetKey, context, transferKey, expiresAt, signature }) {
    return core.verify_transfer_request(bytes(targetKey), context.map((c) => uuid(c)), bytes(transferKey), BigInt(expiresAt), bytes(signature));
  },
  comparisonCode({ context, transferKey, sourceKey, targetKey }) {
    return core.comparison_code(context.map((c) => uuid(c)), bytes(transferKey), bytes(sourceKey), bytes(targetKey));
  },
  /** SOURCE: seals this device's decrypted history (bounded) to the target's key. */
  async sealHistory({ context, transferKey }) {
    const device = requireDevice();
    const d = await db();
    const range = IDBKeyRange.bound([ctx.account, "", 0], [ctx.account, "ï¿¿", Number.MAX_SAFE_INTEGER]);
    const store = d.transaction("records").objectStore("records");
    const keys = await req(store.getAllKeys(range));
    const values = await req(store.getAll(range));
    const conversations = {};
    let size = 0;
    for (let i = values.length - 1; i >= 0; i--) {
      const [, conv, seq] = keys[i];
      const r = await redactRemovedRecord(conv, JSON.parse(dec.decode(await unseal(ctx.key, recordAad(ctx.account, conv, seq), values[i]))));
      if (!["DECRYPTED", "SENT"].includes(r.status)) continue;
      const line = JSON.stringify(r);
      if (size + enc.encode(line).length > MAX_ARCHIVE_BYTES) fail("HISTORY_REQUIRES_PAGINATION");
      size += enc.encode(line).length;
      (conversations[conv] ??= []).push(r);
    }
    if (!Object.keys(conversations).length) fail("HISTORY_SOURCE_EMPTY");
    const archive = enc.encode(JSON.stringify({ v: 1, conversations }));
    const sealed = device.sealHistory(context.map((c) => uuid(c)), bytes(transferKey), archive);
    archive.fill(0);
    return { sealed, conversations: Object.keys(conversations).length };
  },
  /** TARGET: verifies the source device and stores the received history. */
  async openHistory({ sourceKey, context, sealed }) {
    requireDevice();
    const plain = requireDevice().openHistory(bytes(sourceKey), context.map((c) => uuid(c)), bytes(sealed));
    let archive;
    try {
      archive = JSON.parse(dec.decode(plain));
    } catch {
      fail("ARCHIVE_INVALID");
    }
    if (archive?.v !== 1 || typeof archive.conversations !== "object") fail("ARCHIVE_INVALID");
    const puts = [];
    let n = 0;
    for (const [conv, records] of Object.entries(archive.conversations)) {
      uuid(conv);
      for (const r of records) {
        if (!Number.isSafeInteger(r.sequence) || !["DECRYPTED", "SENT"].includes(r.status)) continue;
        const rec = await redactRemovedRecord(conv, { ...r, status: r.status === "SENT" ? "SENT" : "DECRYPTED", transferred: true });
        puts.push(["records", [ctx.account, conv, r.sequence], await seal(ctx.key, recordAad(ctx.account, conv, r.sequence), enc.encode(JSON.stringify(rec)))]);
        n++;
      }
    }
    await persistDevice(puts);
    return { records: n, conversations: Object.keys(archive.conversations).length };
  },

  /**
   * A message written while no other member of an encrypted conversation has a device yet
   * ("pending setup"). Kept sealed at rest (never in the clear, never sent) until the
   * conversation can encrypt it for a recipient device; then sent and dropped.
   */
  async holdDraft({ conversationId, clientMessageId, text, createdAt }) {
    requireDevice();
    uuid(conversationId);
    uuid(clientMessageId);
    if (typeof text !== "string" || text.length < 1 || text.length > MAX_TEXT) fail("TEXT_INVALID");
    const value = { text, createdAt: typeof createdAt === "string" ? createdAt.slice(0, 40) : new Date().toISOString() };
    const sealed = await seal(ctx.key, draftAad(ctx.account, conversationId, clientMessageId), enc.encode(JSON.stringify(value)));
    const d = await db();
    const tx = d.transaction("pending", "readwrite");
    tx.objectStore("pending").put(sealed, draftKey(ctx.account, conversationId, clientMessageId));
    await done(tx);
    return true;
  },

  /** The held drafts of a conversation, oldest first. */
  async drafts({ conversationId }) {
    requireDevice();
    uuid(conversationId);
    const prefix = `draft:${conversationId}:`;
    const d = await db();
    const store = d.transaction("pending").objectStore("pending");
    const range = IDBKeyRange.bound([ctx.account, prefix], [ctx.account, prefix + "\uffff"]);
    const keys = await req(store.getAllKeys(range));
    const values = await req(store.getAll(range));
    const out = [];
    for (let i = 0; i < keys.length; i++) {
      const cmid = keys[i][1].slice(prefix.length);
      const plain = await unseal(ctx.key, draftAad(ctx.account, conversationId, cmid), values[i]);
      const v = JSON.parse(dec.decode(plain));
      out.push({ clientMessageId: cmid, text: v.text, createdAt: v.createdAt });
    }
    return out.sort((a, b) => (a.createdAt < b.createdAt ? -1 : a.createdAt > b.createdAt ? 1 : 0));
  },

  async dropDraft({ conversationId, clientMessageId }) {
    requireDevice();
    uuid(conversationId);
    uuid(clientMessageId);
    const d = await db();
    const tx = d.transaction("pending", "readwrite");
    tx.objectStore("pending").delete(draftKey(ctx.account, conversationId, clientMessageId));
    await done(tx);
    return true;
  },

  /** Logout / revocation: everything of the account in this browser is erased. */
  async wipe({ account }) {
    uuid(account);
    const d = await db();
    const tx = d.transaction(["keys", "devices", "records", "pending"], "readwrite");
    tx.objectStore("keys").delete(account);
    tx.objectStore("devices").delete(account);
    const range = IDBKeyRange.bound([account], [account, []]);
    tx.objectStore("records").delete(range);
    tx.objectStore("pending").delete(range);
    await done(tx);
    ctx?.device?.free?.();
    ctx = null;
    handles.clear();
    return true;
  },
};

let operations = Promise.resolve();
function dispatch({ data }, reply) {
  const respond = (message) => { try { reply.postMessage(message); } catch { /* A closed tab must not break other ports' queue. */ } };
  operations = operations.then(async () => {
  const { id, op, args } = data ?? {};
  try {
    if (typeof op !== "string" || !Object.hasOwn(ops, op)) fail("UNKNOWN_OP");
    if (op !== "open" && op !== "wipe") await loadCore();
    const result = await ops[op](args ?? {});
    respond({ id, ok: true, result });
  } catch (e) {
    // Stable codes only: never key material, never a stack.
    const code = e instanceof CodedError || (e instanceof Error && /^[A-Z][A-Z0-9_]{2,63}$/.test(e.message)) ? e.message : "CRYPTO_ERROR";
    respond({ id, ok: false, error: code });
  }
  });
}
self.onmessage = (event) => dispatch(event, self);
self.onconnect = (event) => {
  for (const port of event.ports) {
    port.onmessage = (message) => dispatch(message, port);
    port.start();
  }
};
