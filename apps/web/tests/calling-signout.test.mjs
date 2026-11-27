// S-10 / Notification Service roadmap N0-11: logout revokes this browser's Calling endpoint
// (DELETE /v1/calling/endpoints/{id}, the call the devices page already makes), forgets the
// stored endpoint id, and never hangs or fails because the revoke failed.
// Run: node --experimental-strip-types --test tests/calling-signout.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  ENDPOINT_ID_PREFIX,
  SIGNING_OUT_KEY,
  SIGNING_OUT_MAX_AGE_MS,
  clearSigningOut,
  forgetEndpointId,
  isSigningOut,
  rememberEndpointId,
  signOutCallingEndpoint,
} from "../src/lib/calling/endpoint-signout.ts";

const PRINCIPAL = "11111111-1111-4111-8111-111111111111";
const ENDPOINT = "22222222-2222-4222-8222-222222222222";

function memoryStorage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    get length() { return map.size; },
    key: (i) => [...map.keys()][i] ?? null,
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => { map.set(k, String(v)); },
    removeItem: (k) => { map.delete(k); },
    map,
  };
}

test("logout revokes the stored endpoint and clears the stored id", async () => {
  const storage = memoryStorage({ "cfmeet.calling.endpoint.other": "endpoint-key-stays", "unrelated": "x" });
  rememberEndpointId(storage, PRINCIPAL, ENDPOINT);
  assert.equal(storage.getItem(ENDPOINT_ID_PREFIX + PRINCIPAL), ENDPOINT);
  const revoked = [];
  const ids = await signOutCallingEndpoint({ storage, revoke: async (id) => { revoked.push(id); } });
  assert.deepEqual(revoked, [ENDPOINT]);
  assert.deepEqual(ids, [ENDPOINT]);
  assert.equal(storage.getItem(ENDPOINT_ID_PREFIX + PRINCIPAL), null, "stored id cleared");
  assert.equal(storage.getItem("unrelated"), "x", "other keys untouched");
  assert.equal(storage.getItem("cfmeet.calling.endpoint.other"), "endpoint-key-stays");
});

test("logout completes and still clears the id when the revoke fails", async () => {
  const storage = memoryStorage();
  rememberEndpointId(storage, PRINCIPAL, ENDPOINT);
  const ids = await signOutCallingEndpoint({ storage, revoke: async () => { throw Object.assign(new Error("boom"), { status: 503 }); } });
  assert.deepEqual(ids, [ENDPOINT]);
  assert.equal(storage.getItem(ENDPOINT_ID_PREFIX + PRINCIPAL), null);
});

test("logout completes and still clears the id when the revoke throws synchronously", async () => {
  const storage = memoryStorage();
  rememberEndpointId(storage, PRINCIPAL, ENDPOINT);
  await signOutCallingEndpoint({ storage, revoke: () => { throw new Error("sync"); } });
  assert.equal(storage.getItem(ENDPOINT_ID_PREFIX + PRINCIPAL), null);
});

test("logout is bounded: a revoke that never answers times out", async () => {
  const storage = memoryStorage();
  rememberEndpointId(storage, PRINCIPAL, ENDPOINT);
  let called = 0;
  const started = Date.now();
  await signOutCallingEndpoint({ storage, timeoutMs: 50, revoke: () => { called++; return new Promise(() => {}); } });
  assert.equal(called, 1);
  assert.ok(Date.now() - started < 2000, "did not wait for the hanging revoke");
  assert.equal(storage.getItem(ENDPOINT_ID_PREFIX + PRINCIPAL), null);
});

test("nothing stored (receiving off, never registered): no revoke call", async () => {
  const storage = memoryStorage();
  rememberEndpointId(storage, PRINCIPAL, ENDPOINT);
  forgetEndpointId(storage, PRINCIPAL);
  let called = 0;
  assert.deepEqual(await signOutCallingEndpoint({ storage, revoke: async () => { called++; } }), []);
  assert.equal(called, 0);
});

test("storage unavailable or throwing never breaks logout", async () => {
  assert.deepEqual(await signOutCallingEndpoint({ storage: null, revoke: async () => assert.fail("no revoke") }), []);
  const broken = { get length() { throw new Error("SecurityError"); }, key() { throw new Error(); }, getItem() { throw new Error(); }, setItem() { throw new Error(); }, removeItem() { throw new Error(); } };
  assert.deepEqual(await signOutCallingEndpoint({ storage: broken, revoke: async () => assert.fail("no revoke") }), []);
  assert.equal(isSigningOut(broken), false);
  rememberEndpointId(broken, PRINCIPAL, ENDPOINT);
  forgetEndpointId(broken, PRINCIPAL);
  clearSigningOut(broken);
});

test("while logout runs no tab registers again; the next sign-in clears the marker", async () => {
  const storage = memoryStorage();
  rememberEndpointId(storage, PRINCIPAL, ENDPOINT);
  assert.equal(isSigningOut(storage), false);
  await signOutCallingEndpoint({ storage, revoke: async () => {}, now: () => 1_000_000 });
  assert.equal(isSigningOut(storage, 1_000_000 + 10), true);
  assert.equal(isSigningOut(storage, 1_000_000 + SIGNING_OUT_MAX_AGE_MS), false, "a stale marker never blocks forever");
  clearSigningOut(storage);
  assert.equal(storage.getItem(SIGNING_OUT_KEY), null);
  assert.equal(isSigningOut(storage, 1_000_000 + 10), false);
});

// ---- Wiring: the app uses the existing revoke, before the session is cleared ----

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

test("the browser wrapper revokes through the existing endpointsService.revoke (DELETE /v1/calling/endpoints/{id})", () => {
  const service = read("../src/services/calling/endpoints.service.ts");
  assert.match(service, /revoke: async \(id: string\) =>\s*\(await callingApi\.delete<Endpoint>\(`\/v1\/calling\/endpoints\/\$\{encodeURIComponent\(id\)\}`\)\)\.data/);
  const wrapper = read("../src/lib/calling/signout.ts");
  assert.match(wrapper, /from "@\/services\/calling\/endpoints\.service"/);
  assert.match(wrapper, /revoke: \(id\) => endpointsService\.revoke\(id\)/);
});

test("logout and logout-everywhere revoke the Calling endpoint before the session is ended", () => {
  const auth = read("../src/services/auth.service.ts");
  const body = (name) => {
    const start = auth.indexOf(`${name}: async`);
    assert.ok(start >= 0, name);
    return auth.slice(start, auth.indexOf("\n  },", start));
  };
  const logout = body("logout");
  assert.ok(logout.indexOf("signOutCalling()") >= 0, "logout signs Calling out");
  assert.ok(logout.indexOf("signOutCalling()") < logout.indexOf('"/auth/logout"'), "before the API session ends");
  assert.ok(logout.indexOf("signOutCalling()") < logout.indexOf("forgetLocalSession"), "before the browser session is cleared");
  const everywhere = body("logoutEverywhere");
  assert.ok(everywhere.indexOf("signOutCalling()") >= 0);
  assert.ok(everywhere.indexOf("signOutCalling()") < everywhere.indexOf('"/me/sessions/revoke-all"'));
  assert.match(auth, /allowCallingRegistration\(\)/, "a fresh sign-in registers again");
});

test("the device remembers the registered endpoint id and refuses to register during logout", () => {
  const device = read("../src/hooks/calling/useCallingDevice.ts");
  assert.match(device, /rememberEndpointId\(browserStorage\(\), principalId, registered\.endpointId\)/);
  assert.ok(device.indexOf("isSigningOut(browserStorage())") < device.indexOf("endpointsService.register("), "checked before registering");
  assert.match(device, /forgetEndpointId\(browserStorage\(\), principalId\)/, "turning receiving off forgets the id");
});
