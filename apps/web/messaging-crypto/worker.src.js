// CF Meet Messaging device worker — shipped source of public/messaging-crypto/crypto-worker.*.js
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
//   (the build pins it), and only then compiled — under this worker's own CSP, the only
//   place where 'wasm-unsafe-eval' is granted.
// * A device is held by one tab at a time (Web Locks): two tabs advancing one MLS state
//   would fork it.
import init, * as core from "__GLUE__";

const WASM_URL = new URL("__WASM__", import.meta.url);
const WASM_SHA384 = "__WASM_SHA384__";
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
    const module = new Uint8Array(await res.arrayBuffer());
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-384", module));
    if (btoa(String.fromCharCode(...digest)) !== WASM_SHA384) fail("CRYPTO_MODULE_INTEGRITY");
    await init({ module_or_path: module });
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

// ----------------------------------------------------------------- state

/** The open account: its device, wrapping key, scope and non-secret metadata. */
let ctx = null;
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
  const envelope = enc.encode(
    JSON.stringify({ deviceId: ctx.deviceId, meta: ctx.meta, state: b64u.enc(device.exportState()) }),
  );
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
    return { t: "text", text: p.text };
  }
  if (p.t === "file" && typeof p.d === "string") {
    const descriptor = b64u.dec(p.d);
    if (!core.is_descriptor(descriptor)) fail("PAYLOAD_INVALID");
    return { t: "file", d: p.d, caption: typeof p.caption === "string" ? p.caption.slice(0, 1024) : null };
  }
  fail("PAYLOAD_INVALID");
}

/** What the page sees of a stored record (never the attachment key). */
function view(r) {
  const out = { ...r };
  if (r.payload?.t === "file") {
    out.payload = { t: "file", caption: r.payload.caption ?? null, attachmentId: r.attachmentId ?? null, name: r.fileName ?? null, mediaType: r.mediaType ?? null };
  }
  return out;
}

async function loadRecord(conversationId, sequence) {
  const d = await db();
  const sealed = await req(d.transaction("records").objectStore("records").get([ctx.account, conversationId, sequence]));
  if (!sealed) return null;
  return JSON.parse(dec.decode(await unseal(ctx.key, recordAad(ctx.account, conversationId, sequence), sealed)));
}

// ------------------------------------------------------------- operations

const ops = {
  /** Opens (or finds no) device of `account` (its principal id) in this browser. */
  async open({ account, scope }) {
    uuid(account);
    for (const k of ["tenantId", "applicationId", "environmentId"]) uuid(scope?.[k]);
    await loadCore();
    if (ctx && ctx.account !== account) fail("ANOTHER_ACCOUNT_OPEN");
    if (!(await acquireLock(account))) return { status: "LOCKED" };
    const d = await db();
    let key = await req(d.transaction("keys").objectStore("keys").get(account));
    if (!key) {
      key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
      const tx = d.transaction("keys", "readwrite");
      tx.objectStore("keys").put(key, account);
      await done(tx);
    }
    if (key.extractable) fail("WRAPPING_KEY_EXTRACTABLE");
    ctx = { account, scope: { ...scope }, key, device: null, deviceId: null, persistent: false, meta: { cursors: {}, kt: null }, snapshot: null };
    const sealed = await req(d.transaction("devices").objectStore("devices").get(account));
    if (!sealed) return { status: "NONE" };
    const envelope = JSON.parse(dec.decode(await unseal(key, deviceAad(account), sealed)));
    const state = b64u.dec(envelope.state);
    ctx.device = core.WebDevice.fromState(uuid(envelope.deviceId), state);
    state.fill(0);
    ctx.deviceId = envelope.deviceId;
    ctx.meta = { cursors: {}, kt: null, ...envelope.meta };
    ctx.persistent = true;
    return { status: "READY", deviceId: ctx.deviceId, signaturePublicKey: ctx.device.signaturePublicKey };
  },

  /** New device keys for the server-assigned device id (not stored until `adopt`). */
  async generate({ deviceId }) {
    if (!ctx) fail("NOT_OPEN");
    if (ctx.persistent) fail("DEVICE_EXISTS");
    ctx.device?.free?.();
    ctx.device = new core.WebDevice(uuid(deviceId));
    ctx.deviceId = deviceId;
    return { deviceId, signaturePublicKey: ctx.device.signaturePublicKey };
  },

  async registrationKeyPackage({ challengeId, nonce }) {
    return requireDevice().registrationKeyPackage(uuid(challengeId), bytes(nonce));
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
    if (!ctx.persistent) fail("DEVICE_NOT_ADOPTED");
    if (!Number.isInteger(count) || count < 1 || count > 20) fail("BAD_COUNT");
    const out = [];
    for (let i = 0; i < count; i++) out.push(device.keyPackage());
    await persistDevice();
    return out;
  },

  async status() {
    return ctx?.device
      ? { status: ctx.persistent ? "READY" : "PENDING", deviceId: ctx.deviceId, signaturePublicKey: ctx.device.signaturePublicKey, cursors: ctx.meta.cursors }
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
    return Number(device.createGroup(bytes(groupId)));
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
    return { commit: r.commit, welcome: r.welcome ?? null, epoch: Number(r.epoch) };
  },
  async removeDevices({ groupId, deviceIds }) {
    const device = requireDevice();
    ctx.snapshot ??= device.exportState();
    const r = device.removeDevices(bytes(groupId), deviceIds.map((d) => uuid(d)));
    return { commit: r.commit, epoch: Number(r.epoch) };
  },
  /** The Delivery Service accepted the change: make it durable. */
  async confirm() {
    ctx.snapshot?.fill(0);
    ctx.snapshot = null;
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
    return true;
  },

  async join({ groupId, welcome }) {
    const epoch = Number(requireDevice().join(bytes(groupId), bytes(welcome)));
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
    uuid(conversationId);
    uuid(clientMessageId);
    let p;
    let attachmentId = null;
    let fileMeta = {};
    if (payload?.t === "text") {
      if (typeof payload.text !== "string" || payload.text.length < 1 || payload.text.length > MAX_TEXT) fail("TEXT_INVALID");
      p = { v: 1, t: "text", text: payload.text };
    } else if (payload?.t === "file") {
      const h = handles.get(payload.handle) ?? fail("UNKNOWN_ATTACHMENT");
      attachmentId = uuid(payload.attachmentId);
      const bound = h.file.descriptorFor(attachmentId);
      p = { v: 1, t: "file", d: b64u.enc(bound), caption: typeof payload.caption === "string" ? payload.caption.slice(0, 1024) : null };
      fileMeta = { fileName: h.fileName, mediaType: h.mediaType, size: h.size };
      handles.delete(payload.handle);
      h.file.free();
    } else {
      fail("PAYLOAD_INVALID");
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
  async processMessages({ conversationId, groupId, messages }) {
    const device = requireDevice();
    uuid(conversationId);
    const gid = bytes(groupId);
    const cursor = ctx.meta.cursors[conversationId] ?? 0;
    const sorted = [...messages].sort((a, b) => a.sequence - b.sequence);
    const out = [];
    const puts = [];
    const deletes = [];
    let last = cursor;
    for (const m of sorted) {
      if (!Number.isSafeInteger(m.sequence) || m.sequence <= last) continue;
      last = m.sequence;
      const base = { sequence: m.sequence, messageId: m.messageId, senderPrincipalId: m.senderPrincipalId, senderDeviceId: m.senderDeviceId, createdAt: m.createdAt, kind: m.kind };
      let record;
      try {
        if (m.lifecycle && m.lifecycle !== "ACTIVE") {
          record = { ...base, status: "REMOVED", lifecycle: m.lifecycle };
        } else if (m.kind === "MLS_COMMIT") {
          const own = m.senderDeviceId === ctx.deviceId;
          const epoch = Number(device.epoch(gid));
          if (own || m.epoch < epoch) {
            record = { ...base, status: "COMMIT", epoch };
          } else {
            const r = device.process(gid, b64u.dec(m.ciphertext), undefined);
            if (r.kind !== "COMMIT") fail("UNEXPECTED_MESSAGE");
            record = { ...base, status: "COMMIT", epoch: Number(r.epoch) };
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
        } else if (m.epoch < Number(device.epoch(gid))) {
          record = { ...base, status: "BEFORE_JOIN" };
        } else {
          const r = device.process(gid, b64u.dec(m.ciphertext), aadFor(conversationId, uuid(m.clientMessageId), uuid(m.senderDeviceId)));
          if (r.kind !== "APPLICATION") fail("UNEXPECTED_MESSAGE");
          const payload = parsePayload(r.plaintext);
          r.plaintext.fill(0);
          record = { ...base, status: "DECRYPTED", payload, attachmentId: (m.attachmentIds ?? [])[0] ?? null };
          if (payload.t === "file") {
            // Presentation metadata comes from the descriptor (protected), never the server.
            const info = core.descriptor_info(b64u.dec(payload.d));
            Object.assign(record, { fileName: info.fileName, mediaType: info.mediaType, size: Number(info.size) });
            info.free();
          }
        }
      } catch (e) {
        record = { ...base, status: "FAILED", error: e instanceof Error ? String(e.message).slice(0, 64) : "FAILED" };
      }
      puts.push(["records", [ctx.account, conversationId, m.sequence], await seal(ctx.key, recordAad(ctx.account, conversationId, m.sequence), enc.encode(JSON.stringify(record)))]);
      out.push(view(record));
    }
    ctx.meta.cursors[conversationId] = last;
    await persistDevice(puts, deletes);
    return { cursor: last, records: out };
  },

  async cursor({ conversationId }) {
    return ctx?.meta?.cursors?.[uuid(conversationId)] ?? 0;
  },

  /** Stored history of a conversation, newest last (at most `limit`). */
  async history({ conversationId, limit = 200 }) {
    if (!ctx) fail("NOT_OPEN");
    uuid(conversationId);
    const d = await db();
    const range = IDBKeyRange.bound([ctx.account, conversationId, 0], [ctx.account, conversationId, Number.MAX_SAFE_INTEGER]);
    const store = d.transaction("records").objectStore("records");
    const keys = await req(store.getAllKeys(range));
    const values = await req(store.getAll(range));
    const out = [];
    for (let i = Math.max(0, values.length - limit); i < values.length; i++) {
      const seq = keys[i][2];
      out.push(view(JSON.parse(dec.decode(await unseal(ctx.key, recordAad(ctx.account, conversationId, seq), values[i])))));
    }
    return out;
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
    const r = await loadRecord(uuid(conversationId), sequence);
    if (!r || r.payload?.t !== "file" || !r.attachmentId) fail("NO_SUCH_ATTACHMENT");
    const f = core.decrypt_file(b64u.dec(r.payload.d), r.attachmentId, bytes(ciphertext));
    return { data: f.plaintext, mediaType: f.mediaType, fileName: f.fileName };
  },

  // --------------------------------------------------- history transfer (AD-07)

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
    const range = IDBKeyRange.bound([ctx.account, "", 0], [ctx.account, "￿", Number.MAX_SAFE_INTEGER]);
    const store = d.transaction("records").objectStore("records");
    const keys = await req(store.getAllKeys(range));
    const values = await req(store.getAll(range));
    const conversations = {};
    let size = 0;
    for (let i = values.length - 1; i >= 0; i--) {
      const [, conv, seq] = keys[i];
      const r = JSON.parse(dec.decode(await unseal(ctx.key, recordAad(ctx.account, conv, seq), values[i])));
      if (!["DECRYPTED", "SENT"].includes(r.status)) continue;
      const line = JSON.stringify(r);
      if (size + line.length > MAX_ARCHIVE_BYTES) break;
      size += line.length;
      (conversations[conv] ??= []).push(r);
    }
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
        const rec = { ...r, status: r.status === "SENT" ? "SENT" : "DECRYPTED", transferred: true };
        puts.push(["records", [ctx.account, conv, r.sequence], await seal(ctx.key, recordAad(ctx.account, conv, r.sequence), enc.encode(JSON.stringify(rec)))]);
        n++;
      }
    }
    await persistDevice(puts);
    return { records: n, conversations: Object.keys(archive.conversations).length };
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

self.onmessage = async ({ data }) => {
  const { id, op, args } = data ?? {};
  try {
    if (typeof op !== "string" || !Object.hasOwn(ops, op)) fail("UNKNOWN_OP");
    if (op !== "open" && op !== "wipe") await loadCore();
    const result = await ops[op](args ?? {});
    self.postMessage({ id, ok: true, result });
  } catch (e) {
    // Stable codes only: never key material, never a stack.
    const code = e instanceof CodedError || (e instanceof Error && /^[A-Z][A-Z0-9_]{2,63}$/.test(e.message)) ? e.message : "CRYPTO_ERROR";
    self.postMessage({ id, ok: false, error: code });
  }
};
