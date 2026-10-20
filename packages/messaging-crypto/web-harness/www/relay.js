// The page plays the untrusted Delivery Service / object store: it only relays bytes
// between device workers and never holds a key. window.runJourney() returns a step log.

// Trusted Types (the page is served with require-trusted-types-for 'script'): the only
// script URL this page may create is the device worker.
const workerUrl = window.trustedTypes
  ? window.trustedTypes
      .createPolicy("cfmeet-harness", {
        createScriptURL: (u) => {
          if (u !== "./device-worker.js") throw new TypeError(`refused script URL ${u}`);
          return u;
        },
      })
      .createScriptURL("./device-worker.js")
  : "./device-worker.js";

let seq = 0;
class DeviceHandle {
  constructor(label) {
    this.label = label;
    this.worker = new Worker(workerUrl, { type: "module" });
    this.pending = new Map();
    this.worker.onmessage = ({ data }) => {
      const p = this.pending.get(data.id);
      this.pending.delete(data.id);
      data.ok ? p.resolve(data.result) : p.reject(new Error(data.error));
    };
    // A worker that fails to load or crashes fails every pending call (never hangs).
    this.worker.onerror = (e) => {
      for (const p of this.pending.values()) p.reject(new Error(`worker error: ${e.message ?? "load failed"}`));
      this.pending.clear();
      this.failed = e.message ?? "load failed";
    };
  }
  send(op, name, args) {
    if (this.failed) return Promise.reject(new Error(`worker error: ${this.failed}`));
    const id = ++seq;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({ id, op, name, args });
    });
  }
  async generate(deviceId) {
    const r = await this.send("generate", null, [deviceId]);
    Object.assign(this, r);
    return r;
  }
  call(name, ...args) {
    return this.send("device", name, args);
  }
  core(name, ...args) {
    return this.send("core", name, args);
  }
}

const enc = new TextEncoder();
const dec = new TextDecoder();
const rand = (n) => {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i += 65536) crypto.getRandomValues(out.subarray(i, Math.min(n, i + 65536)));
  return out;
};
const eq = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
const tamper = (b, at = b.length - 1) => {
  const c = Uint8Array.from(b);
  c[at] ^= 0x01;
  return c;
};
async function refused(promise) {
  try {
    await promise;
    return null;
  } catch (e) {
    return e.message;
  }
}

window.runJourney = async function runJourney() {
  const log = [];
  const step = (name, detail) => log.push({ name, ...detail, at: Math.round(performance.now()) });
  const check = (cond, what) => {
    if (!cond) throw new Error(`FAILED: ${what}`);
  };
  const [tenant, application, environment, conversation, principalBob] = Array.from({ length: 5 }, () => crypto.randomUUID());
  const groupId = rand(32);
  const aadFor = (h, mid) => h.core("message_aad", tenant, application, environment, conversation, mid, h.deviceId);

  // 1. Device registration: keys generated in each worker; the registration KeyPackage
  //    answers a server challenge and passes Messaging's own registration checks.
  const alice = new DeviceHandle("alice"), bob = new DeviceHandle("bob"), carol = new DeviceHandle("carol");
  const bobNew = new DeviceHandle("bob-new-device");
  for (const h of [alice, bob, carol, bobNew]) await h.generate(crypto.randomUUID());
  check(new Set([alice, bob, carol, bobNew].map((h) => h.signaturePublicKey.join())).size === 4, "distinct device keys");
  const now = Math.floor(Date.now() / 1000);
  for (const h of [alice, bob, carol, bobNew]) {
    const challenge = crypto.randomUUID(), nonce = rand(32);
    const kp = await h.call("registrationKeyPackage", challenge, nonce);
    check((await h.core("server_check_registration", kp, BigInt(now), challenge, nonce, h.deviceId)) === h.deviceId, "registration accepted");
    const other = await refused(h.core("server_check_registration", kp, BigInt(now), crypto.randomUUID(), nonce, h.deviceId));
    check(other === "REGISTRATION_BINDING_MISMATCH", `a KeyPackage bound to another challenge is refused (${other})`);
  }
  step("device registration", { devices: 4 });

  // 2. KeyPackage publication, group creation, Welcome processing.
  check((await alice.call("createGroup", groupId)) === 0, "epoch 0");
  const addBob = await alice.call("addDevice", groupId, await bob.call("keyPackage"));
  check((await bob.call("join", addBob.welcome)) === 1, "bob joined at epoch 1");
  step("group create + welcome", { epoch: 1, members: (await alice.call("members", groupId)).length });

  // 3. Encrypt / decrypt both ways with the canonical AAD.
  const m1 = crypto.randomUUID();
  const aad1 = await aadFor(alice, m1);
  const ct1 = await alice.call("encrypt", groupId, aad1, enc.encode("hello bob"));
  const got1 = await bob.call("process", groupId, ct1);
  check(got1.kind === "APPLICATION" && dec.decode(got1.plaintext) === "hello bob" && eq(got1.aad, aad1), "bob decrypts");
  check(await bob.core("aad_is_canonical", got1.aad), "canonical AAD");
  check(!dec.decode(ct1).includes("hello bob"), "ciphertext hides plaintext");
  const ct2 = await bob.call("encrypt", groupId, await aadFor(bob, crypto.randomUUID()), enc.encode("hi alice"));
  check(dec.decode((await alice.call("process", groupId, ct2)).plaintext) === "hi alice", "alice decrypts");
  step("encrypt/decrypt", { messages: 2 });

  // 4. Tampered ciphertext and replay are rejected.
  const tamperedErr = await refused(alice.call("process", groupId, tamper(ct2)));
  check(tamperedErr !== null, "tampered ciphertext refused");
  const replayErr = await refused(bob.call("process", groupId, ct1));
  check(replayErr !== null, "replayed ciphertext refused");
  step("tamper + replay rejected", { tamperedErr, replayErr });

  // 5. Key substitution: a KeyPackage whose signature was altered is refused, nothing changes.
  const carolKp = await carol.call("keyPackage");
  const subErr = await refused(alice.call("addDevice", groupId, tamper(carolKp)));
  check(subErr !== null && (await alice.call("epoch", groupId)) === 1, "tampered KeyPackage refused, epoch unchanged");
  step("key substitution rejected", { subErr });

  // 6. Membership update / epoch change: carol joins; she cannot read pre-join history.
  const addCarol = await alice.call("addDevice", groupId, carolKp);
  check((await bob.call("process", groupId, addCarol.commit)).epoch === 2, "bob merges commit");
  check((await carol.call("join", addCarol.welcome)) === 2, "carol at epoch 2");
  const oldErr = await refused(carol.call("process", groupId, ct1));
  check(oldErr !== null, "no pre-join history for a new member");
  step("epoch change", { epoch: 2, oldErr });

  // 7. Offline delivery: carol is offline while three messages are sent; later in order.
  const queue = [];
  for (const t of ["one", "two", "three"]) queue.push(await alice.call("encrypt", groupId, await aadFor(alice, crypto.randomUUID()), enc.encode(t)));
  for (const c of queue) await bob.call("process", groupId, c);
  const offline = [];
  for (const c of queue) offline.push(dec.decode((await carol.call("process", groupId, c)).plaintext));
  check(offline.join() === "one,two,three", "offline delivery in order");
  step("offline delivery", { queued: queue.length });

  // 8. Attachment encrypt / decrypt: the file key travels inside an MLS message.
  const file = rand(300_000);
  const attachmentId = crypto.randomUUID();
  const ef = await alice.core("encrypt_file", file, "image/png", "photo.png", attachmentId);
  check(ef.ciphertext.length > file.length && !eq(ef.ciphertext.subarray(64, 96), file.subarray(0, 32)), "file ciphertext");
  const carrier = await alice.call("encrypt", groupId, await aadFor(alice, crypto.randomUUID()), ef.descriptor);
  await bob.call("process", groupId, carrier);
  const descriptor = (await carol.call("process", groupId, carrier)).plaintext;
  check(eq(await carol.core("decrypt_file", descriptor, ef.ciphertext), file), "carol decrypts the file");
  const fileErr = await refused(carol.core("decrypt_file", descriptor, tamper(ef.ciphertext, 100)));
  check(fileErr !== null, "tampered file refused");
  step("attachment", { bytes: file.length, ciphertextBytes: ef.ciphertext.length, fileErr });

  // 9. History recovery: bob's NEW device obtains bob's history from bob's old device
  //    (authenticated device-to-device transfer, no server key).
  const transferId = crypto.randomUUID();
  const ctx = [transferId, tenant, application, environment, principalBob, bob.deviceId, bobNew.deviceId];
  const tk = await bobNew.call("beginTransfer");
  const expires = BigInt(now + 600);
  const request = await bobNew.call("signTransferRequest", ctx, expires);
  check(await alice.core("verify_transfer_request", bobNew.signaturePublicKey, ctx, tk, expires, request), "server verifies the request");
  check(!(await alice.core("verify_transfer_request", carol.signaturePublicKey, ctx, tk, expires, request)), "request from another device key refused");
  const codeSource = await bob.core("comparison_code", ctx, tk, bob.signaturePublicKey, bobNew.signaturePublicKey);
  const codeTarget = await bobNew.core("comparison_code", ctx, tk, bob.signaturePublicKey, bobNew.signaturePublicKey);
  check(codeSource === codeTarget && /^\d{4} \d{4}$/.test(codeSource), "comparison codes match");
  const archive = enc.encode(JSON.stringify({ conversation, messages: ["hello bob", "hi alice", ...offline] }));
  const sealed = await bob.call("sealHistory", ctx, tk, archive);
  check(!dec.decode(sealed).includes("hello bob"), "sealed archive hides history");
  // A substituted source (another device seals) and a cross-tenant context are refused,
  // each with a fresh single-use transfer key.
  const forged = await carol.call("sealHistory", ctx, tk, archive);
  const forgedErr = await refused(bobNew.call("openHistory", bob.signaturePublicKey, ctx, forged));
  check(forgedErr === "BadSignature", `forged source refused (${forgedErr})`);
  const tk2 = await bobNew.call("beginTransfer");
  const sealed2 = await bob.call("sealHistory", ctx, tk2, archive);
  const crossErr = await refused(bobNew.call("openHistory", bob.signaturePublicKey, [ctx[0], crypto.randomUUID(), ...ctx.slice(2)], sealed2));
  check(crossErr !== null, "cross-tenant context refused");
  const tk3 = await bobNew.call("beginTransfer");
  const sealed3 = await bob.call("sealHistory", ctx, tk3, archive);
  const tamperedSealErr = await refused(bobNew.call("openHistory", bob.signaturePublicKey, ctx, tamper(sealed3, 20)));
  check(tamperedSealErr !== null, "tampered archive refused");
  const tk4 = await bobNew.call("beginTransfer");
  const staleErr = await refused(bobNew.call("openHistory", bob.signaturePublicKey, ctx, sealed3));
  check(staleErr !== null, "an archive sealed to an earlier transfer key is refused");
  const tk5 = await bobNew.call("beginTransfer");
  const good = await bob.call("sealHistory", ctx, tk5, archive);
  const opened = await bobNew.call("openHistory", bob.signaturePublicKey, ctx, good);
  check(eq(opened, archive), "history recovered on the new device");
  const replayTransfer = await refused(bobNew.call("openHistory", bob.signaturePublicKey, ctx, good));
  check(replayTransfer === "NO_TRANSFER", "transfer key is single-use");
  void tk4;
  step("history transfer", { comparisonCode: "#### ####", forgedErr, crossErr, tamperedSealErr, staleErr, replayTransfer });

  // 10. Device revocation: bob's device is removed; it cannot read later messages.
  const rm = await alice.call("removeDevice", groupId, bob.deviceId);
  check((await carol.call("process", groupId, rm.commit)).epoch === 3, "carol merges removal");
  const bobOnRemoval = await refused(bob.call("process", groupId, rm.commit));
  const after = await alice.call("encrypt", groupId, await aadFor(alice, crypto.randomUUID()), enc.encode("after removal"));
  check(dec.decode((await carol.call("process", groupId, after)).plaintext) === "after removal", "carol reads");
  const bobAfter = await refused(bob.call("process", groupId, after));
  check(bobAfter !== null, "removed device cannot decrypt");
  const members = await alice.call("members", groupId);
  check(members.length === 2 && !members.includes(bob.deviceId), "bob gone from the MLS tree");
  step("device revocation", { epoch: 3, bobOnRemoval, bobAfter });

  // 11. No key material persisted in browser storage.
  const dbs = indexedDB.databases ? await indexedDB.databases() : [];
  const cacheKeys = await caches.keys();
  check(localStorage.length === 0 && sessionStorage.length === 0 && dbs.length === 0 && cacheKeys.length === 0, "no persisted secrets");
  step("no browser persistence", { localStorage: localStorage.length, indexedDB: dbs.length, caches: cacheKeys.length });

  for (const h of [alice, bob, carol, bobNew]) h.worker.terminate();
  return log;
};
window.harnessReady = true;
