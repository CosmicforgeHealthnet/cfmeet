// TRANSITIONAL (one release, remove after #193 is deployed): the Status / Channels fallback to
// /api/v2/social/* triggers ONLY on the Meeting relay's route-not-allowed answer
// (404 ENDPOINT_NOT_FOUND), never on a real 404 NOT_FOUND or a 403 privacy denial; the choice
// is remembered for the session and switches back once the backend is deployed.
// Run: node --experimental-strip-types --test tests/social-backend.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { createBackendSwitch, isRouteNotAllowed } from "../src/services/v2/social-backend.ts";
import { createSwitchingTransport } from "../src/lib/status-publisher/switching-transport.ts";

/** The Meeting relay refusing a route it does not allow (MessagingApiError shape). */
const routeNotAllowed = () => Object.assign(new Error("ENDPOINT_NOT_FOUND"), { status: 404, code: "ENDPOINT_NOT_FOUND" });
/** The same answer from an axios call to a removed /api/v2/social route. */
const axiosRouteGone = () => Object.assign(new Error("Request failed"), { response: { status: 404, data: { error: { code: "ENDPOINT_NOT_FOUND" } } } });
const notFound = () => Object.assign(new Error("NOT_FOUND"), { status: 404, code: "NOT_FOUND" });
const privacyDenied = () => Object.assign(new Error("PERMISSION_DENIED"), { status: 403, code: "PERMISSION_DENIED" });
const forbiddenRouteCode = () => Object.assign(new Error("x"), { status: 403, code: "ENDPOINT_NOT_FOUND" });

function memoryStore(initial = {}) {
  const m = new Map(Object.entries(initial));
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, v), map: m };
}

test("only 404 ENDPOINT_NOT_FOUND counts as route-not-allowed", () => {
  assert.equal(isRouteNotAllowed(routeNotAllowed()), true);
  assert.equal(isRouteNotAllowed(axiosRouteGone()), true);
  assert.equal(isRouteNotAllowed(notFound()), false);
  assert.equal(isRouteNotAllowed(privacyDenied()), false);
  assert.equal(isRouteNotAllowed(forbiddenRouteCode()), false);
  assert.equal(isRouteNotAllowed({ response: { status: 404, data: { error: { code: "NOT_FOUND" } } } }), false);
  assert.equal(isRouteNotAllowed(new Error("network")), false);
  assert.equal(isRouteNotAllowed(null), false);
});

test("falls back to legacy on route-not-allowed and remembers it for the session", async () => {
  const store = memoryStore();
  const b = createBackendSwitch(store);
  let modernCalls = 0;
  let legacyCalls = 0;
  const modern = async () => { modernCalls++; throw routeNotAllowed(); };
  const legacy = async () => { legacyCalls++; return "legacy"; };
  assert.equal(await b.run(modern, legacy), "legacy");
  assert.equal(b.current(), "legacy");
  assert.equal(store.map.get("cfmeet.social.backend"), "legacy");
  assert.equal(await b.run(modern, legacy), "legacy");
  assert.equal(modernCalls, 1, "the second call goes straight to legacy");
  assert.equal(legacyCalls, 2);
  // A new page in the same session starts on legacy.
  assert.equal(createBackendSwitch(store).current(), "legacy");
});

for (const [name, make] of [["404 NOT_FOUND", notFound], ["403 privacy denial", privacyDenied], ["network error", () => new Error("offline")]]) {
  test(`never falls back on ${name}`, async () => {
    const b = createBackendSwitch(memoryStore());
    let legacyCalls = 0;
    const err = make();
    await assert.rejects(b.run(async () => { throw err; }, async () => { legacyCalls++; return "legacy"; }), (e) => e === err);
    assert.equal(legacyCalls, 0);
    assert.equal(b.current(), "messaging");
  });
}

test("a real legacy error is returned as is (no switch back)", async () => {
  const b = createBackendSwitch(memoryStore({ "cfmeet.social.backend": "legacy" }));
  const err = notFound();
  let modernCalls = 0;
  await assert.rejects(b.run(async () => { modernCalls++; return "m"; }, async () => { throw err; }), (e) => e === err);
  assert.equal(modernCalls, 0);
  assert.equal(b.current(), "legacy");
});

test("switches back to Messaging once the legacy route is gone (backend deployed)", async () => {
  const b = createBackendSwitch(memoryStore({ "cfmeet.social.backend": "legacy" }));
  assert.equal(await b.run(async () => "messaging", async () => { throw axiosRouteGone(); }), "messaging");
  assert.equal(b.current(), "messaging");
});

test("a Messaging-only operation keeps its own error", async () => {
  const b = createBackendSwitch(memoryStore());
  await assert.rejects(b.run(async () => { throw routeNotAllowed(); }), (e) => e.code === "ENDPOINT_NOT_FOUND");
  assert.equal(b.current(), "messaging");
});

function fakeTransport(name, { refuse = false, error = null, idempotentCreate = true } = {}) {
  const calls = [];
  const fail = () => { if (refuse) throw Object.assign({ transportError: true, kind: "rejected", status: 404, message: "x" }, { code: "ENDPOINT_NOT_FOUND" }); if (error) throw error; };
  return {
    calls,
    capabilities: { listParts: !idempotentCreate ? false : true, idempotentCreate, cancelUpload: idempotentCreate },
    async startUpload() { calls.push("start"); fail(); return { uploadId: `${name}-1`, partSize: 4, partCount: 1 }; },
    async storedParts(id) { calls.push(`parts:${id}`); return []; },
    async uploadPart(id) { calls.push(`part:${id}`); },
    async completeUpload(id) { calls.push(`complete:${id}`); return { mediaId: id }; },
    async cancelUpload(id) { calls.push(`cancel:${id}`); },
    async createStatus(req) { calls.push(`create:${req.mediaId ?? req.text}`); fail(); return { postId: `${name}-post` }; },
    async findCreatedStatus() { calls.push("find"); return []; },
  };
}

test("publish queue transport: falls back on route-not-allowed, keeps an upload on its backend, degraded flags on legacy", async () => {
  const b = createBackendSwitch(memoryStore());
  const modern = fakeTransport("m", { refuse: true });
  const legacy = fakeTransport("l", { idempotentCreate: false });
  const t = createSwitchingTransport(modern, legacy, b);
  assert.equal(t.capabilities.idempotentCreate, true);
  const signal = new AbortController().signal;
  const u = await t.startUpload({ kind: "IMAGE", contentType: "image/jpeg", sizeBytes: 3, durationMs: null }, signal);
  assert.equal(u.uploadId, "l-1");
  assert.equal(b.current(), "legacy");
  assert.deepEqual(t.capabilities, legacy.capabilities, "the legacy transport keeps its degraded flags");
  await t.uploadPart(u.uploadId, 0, new Blob(["abc"]), signal);
  await t.completeUpload(u.uploadId, signal);
  assert.deepEqual(await t.createStatus({ kind: "IMAGE", mediaId: "l-1" }, "k1", signal), { postId: "l-post" });
  assert.deepEqual(legacy.calls, ["start", "part:l-1", "complete:l-1", "create:l-1"]);
  assert.deepEqual(modern.calls, ["start"]);
  await t.findCreatedStatus({ kind: "TEXT", text: "x" }, new Date().toISOString(), signal);
  assert.equal(legacy.calls.at(-1), "find");
});

test("publish queue transport: a 404 NOT_FOUND or 403 from Messaging never falls back", async () => {
  for (const status of [404, 403]) {
    const b = createBackendSwitch(memoryStore());
    const err = { transportError: true, kind: status === 404 ? "gone" : "auth", status, message: "x", code: status === 404 ? "NOT_FOUND" : "PERMISSION_DENIED" };
    const modern = fakeTransport("m", { error: err });
    const legacy = fakeTransport("l");
    const t = createSwitchingTransport(modern, legacy, b);
    await assert.rejects(t.createStatus({ kind: "TEXT", text: "hi" }, "k", new AbortController().signal), (e) => e === err);
    assert.deepEqual(legacy.calls, []);
    assert.equal(b.current(), "messaging");
  }
});
