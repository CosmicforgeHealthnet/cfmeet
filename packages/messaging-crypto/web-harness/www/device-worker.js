// One device per Web Worker. The WebDevice (and every private key it holds) never leaves
// this worker: only public keys, MLS messages and ciphertext are posted out.
import init, * as core from "./pkg/cf_messaging_crypto_web_harness.js";

const ready = init();
let device = null;

function plain(v) {
  if (typeof v === "bigint") return Number(v);
  if (v instanceof core.CommitResult) return { commit: v.commit, welcome: v.welcome ?? null, epoch: Number(v.epoch) };
  if (v instanceof core.ProcessResult) return { kind: v.kind, aad: v.aad, plaintext: v.plaintext, epoch: Number(v.epoch) };
  if (v instanceof core.EncryptedFile) return { ciphertext: v.ciphertext, descriptor: v.descriptor, sha256: v.sha256 };
  return v;
}

self.onmessage = async ({ data: { id, op, name, args } }) => {
  try {
    // A failed WebAssembly compile (e.g. a CSP without 'wasm-unsafe-eval') is reported.
    await ready;
    let result;
    if (op === "generate") {
      device = new core.WebDevice(args[0]);
      result = { deviceId: device.deviceId, signaturePublicKey: device.signaturePublicKey };
    } else if (op === "device") {
      if (!(name in core.WebDevice.prototype)) throw new Error(`no device method ${name}`);
      result = device[name](...args);
    } else if (op === "core") {
      if (typeof core[name] !== "function" || name === "default") throw new Error(`no core function ${name}`);
      result = core[name](...args);
    } else {
      throw new Error(`unknown op ${op}`);
    }
    self.postMessage({ id, ok: true, result: plain(result) });
  } catch (e) {
    self.postMessage({ id, ok: false, error: String(e?.message ?? e) });
  }
};
