import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import axios from "axios";

function load(path, dependencies = {}, globals = {}) {
  const exports = {};
  const js = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  vm.runInNewContext(js, { exports, require: (name) => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency ${name}`);
  }, URL, URLSearchParams, process: { env: {} }, ...globals });
  return exports;
}

const routes = load("../src/lib/messaging/routes.ts");
const urls = load("../src/lib/messaging/urls.ts");
const errors = load("../src/lib/messaging/errors.ts");
const id = "11111111-1111-4111-8111-111111111111";

function client({ token = "login-token", expired = false, refreshFails = false } = {}) {
  const requests = [];
  let refreshes = 0;
  const cookieStore = {
    getAccessToken: () => token, getRefreshToken: () => "refresh-token",
    setTokens: (access) => { token = access; },
  };
  const http = load("../src/services/v2/messaging-http.ts", {
    "@/store": { cookieStore }, "@/lib/messaging/errors": errors, "@/lib/messaging/urls": urls,
    axios: {
      isAxiosError: axios.isAxiosError,
      create: (config) => axios.create({ ...config, adapter: async (request) => {
        requests.push(request);
        const response = { status: 200, statusText: "OK", config: request, headers: {}, data: { ok: true } };
        if (expired && request.headers.Authorization === "Bearer login-token") {
          response.status = 401;
          response.data = { error: { code: "AUTHENTICATION_REQUIRED" } };
          throw new axios.AxiosError("Expired", "ERR_BAD_REQUEST", request, null, response);
        }
        return response;
      } }),
      post: async (url, body) => {
        refreshes++;
        assert.equal(url, "http://localhost:8080/api/v1/auth/refresh");
        assert.equal(body.refresh_token, "refresh-token");
        await new Promise((resolve) => setImmediate(resolve));
        if (refreshFails) throw new Error("Refresh refused");
        return { data: { access_token: "new-login", refresh_token: "new-refresh" } };
      },
    },
  });
  const service = load("../src/services/v2/messaging.service.ts", {
    "./messaging-http": http, "@/lib/messaging/errors": errors, "@/lib/messaging/routes": routes,
  });
  const migration = load("../src/services/v2/chat-migration.service.ts", { "./messaging-http": http });
  return { ...service, ...migration, requests, refreshes: () => refreshes };
}

test("default, v1, and v2 configuration all derive the correct bridge and login URLs", () => {
  assert.equal(urls.messagingApiUrls().v2, "http://localhost:8080/api/v2");
  for (const base of ["https://meet.example/api/v1", "https://meet.example/api/v2/"]) {
    const result = urls.messagingApiUrls(base);
    assert.equal(result.v1, "https://meet.example/api/v1");
    assert.equal(result.v2, "https://meet.example/api/v2");
  }
  for (const base of ["http://remote.example/api/v1", "https://user:secret@meet.example/api/v1", "https://meet.example/api/v1?next=evil", "https://meet.example/other"]) {
    assert.throws(() => urls.messagingApiUrls(base));
  }
});

test("all identity/device/session endpoints use the v2 bridge and only the login bearer", async () => {
  const c = client();
  const api = c.messagingService;
  await api.me(); await api.directory("A & B"); await api.resolve([id]); await api.names([id]);
  await api.registrationChallenge(); await api.registerDevice(id, "client-held-proof", "Browser");
  await api.publishKeyPackages(id, ["one-time-key"]); await api.revokeDevice(id, "LOST_OR_STOLEN");
  await api.clientToken(id); await api.revokeAllSessions(); await api.devices(id);
  assert.deepEqual(c.requests.map((r) => [r.method, r.url]), [
    ["get", "/messaging/me"], ["get", "/messaging/directory?q=A%20%26%20B"],
    ["post", "/messaging/directory/resolve"], ["post", "/messaging/principals/names"],
    ["post", "/messaging/devices/registration-challenges"], ["post", "/messaging/devices"],
    ["post", `/messaging/devices/${id}/key-packages`], ["post", `/messaging/devices/${id}/revoke`],
    ["post", "/messaging/client-tokens"], ["post", "/messaging/client-sessions/revoke-all"],
    ["get", `/messaging/relay/v1/messaging/principals/${id}/devices`],
  ]);
  for (const request of c.requests) {
    assert.equal(request.baseURL, "http://localhost:8080/api/v2");
    assert.equal(request.headers.Authorization, "Bearer login-token");
    assert.equal(request.headers["X-Participant-Token"], undefined);
  }
  assert.equal(JSON.parse(c.requests[7].data).reason, "LOST_OR_STOLEN");
  assert.deepEqual(JSON.parse(c.requests[8].data), { deviceId: id });
  assert.equal(c.requests[9].data, undefined);
});

test("concurrent 401s refresh login once on v1 and preserve original command identity", async () => {
  const c = client({ expired: true });
  await Promise.all([c.relay("POST", "v1/conversations", { securityProfile: "MANAGED" }, "stable-command"), c.messagingService.me()]);
  assert.equal(c.refreshes(), 1);
  assert.equal(c.requests.length, 4);
  for (const retried of c.requests.slice(2)) assert.equal(retried.headers.Authorization, "Bearer new-login");
  const creates = c.requests.filter((r) => r.url === "/messaging/relay/v1/conversations");
  assert.equal(creates.length, 2);
  for (const request of creates) assert.equal(request.headers["Idempotency-Key"], "stable-command");
});

test("missing login fails before network and failed refresh does not replay mutations", async () => {
  const anonymous = client({ token: null });
  await assert.rejects(anonymous.messagingService.me(), (e) => e.code === "AUTHENTICATION_REQUIRED");
  assert.equal(anonymous.requests.length, 0);
  const expired = client({ expired: true, refreshFails: true });
  await assert.rejects(expired.messagingService.revokeAllSessions());
  assert.equal(expired.requests.length, 1);
});

test("admin migration operations remain separate and never execute on import", async () => {
  const c = client();
  assert.equal(c.requests.length, 0);
  await c.chatMigrationService.status();
  await c.chatMigrationService.transition({ targetState: "SHADOW_VALIDATION", expectedVersion: 3, reason: "Validate migration" });
  await c.chatMigrationService.runParity();
  assert.deepEqual(c.requests.map((r) => r.url), ["/admin/chat-migration", "/admin/chat-migration/transitions", "/admin/chat-migration/parity-runs"]);
  assert.equal(JSON.parse(c.requests[1].data).expectedVersion, 3);
});

test("the frontend permits every backend end-user relay operation and its query keys", () => {
  const backend = readFileSync(new URL("../../../services/meeting-service/src/services/unified_messaging/relay.rs", import.meta.url), "utf8");
  const block = backend.split("pub static ROUTES")[1].split("/// Matches")[0];
  const starts = [...block.matchAll(/\br\(/g)];
  assert.ok(starts.length >= 30);
  for (const match of starts) {
    let depth = 1, index = match.index + 2;
    const start = index;
    while (depth && index < block.length) {
      if (block[index] === "(") depth++;
      if (block[index] === ")") depth--;
      index++;
    }
    const body = block.slice(start, index - 1);
    const method = body.match(/Method::(\w+)/)[1];
    const segments = body.match(/&\[([\s\S]*?)\]/)[1];
    const path = "v1/" + [...segments.matchAll(/Lit\("([^"]+)"\)|\bId\b/g)].map((m) => m[1] || id).join("/");
    assert.equal(routes.isMessagingRelayRoute(method, path), true, `${method} ${path}`);
    const rest = body.slice(body.indexOf(segments) + segments.length + 1);
    const query = rest.includes("PAGE") ? ["limit", "cursor"] : rest.includes("MESSAGES") ? ["afterSequence", "beforeSequence", "limit"] : [...rest.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    for (const key of query) assert.equal(routes.isMessagingRelayRoute(method, `${path}?${key}=10`), true, `${path}?${key}`);
  }
});

test("relay refuses administration, traversal, wrong methods, and unsupported query keys", async () => {
  const c = client();
  for (const [method, path] of [
    ["GET", "v1/messaging/policy"], ["POST", "v1/messaging/legal-holds"],
    ["GET", "v1/conversations/../policy"], ["GET", `v1/conversations/${id}/%2e%2e/policy`],
    ["DELETE", `v1/conversations/${id}`], ["GET", "https://evil.example/v1/conversations"],
    ["GET", "v1/conversations?principalId=other"], ["GET", "v1/conversations#fragment"],
  ]) await assert.rejects(c.relay(method, path), (error) => error.code === "BAD_PATH");
  assert.equal(c.requests.length, 0);
  assert.equal(routes.isMessagingRelayRoute("GET", "v1/conversations?cursor=abc%2Bdef%3D"), true);
});

test("revoke-all pauses reconnects, preserves client keys, and resumes only on request", async () => {
  const cryptoCalls = [];
  let opened = 0, closed = 0, revocations = 0;
  const fakeCrypto = { call: async (command) => {
    cryptoCalls.push(command);
    if (command === "open") return { status: "READY", deviceId: id };
    throw new Error(`Unexpected crypto operation ${command}`);
  }, terminate() {} };
  class Transport {
    constructor() { opened++; }
    connect() { return Promise.resolve(); }
    close() { closed++; }
    subscribe() {}
  }
  const engineModule = load("../src/lib/messaging/engine.ts", {
    "@/services/messaging-realtime": { MessagingRealtimeTransport: Transport },
    "@/services/messaging-session": { messagingClientAccessToken: () => ({}) },
    "./api": {},
    "./errors": errors,
    "@/services/v2/messaging.service": { messagingService: {
      me: async () => ({ principalId: id, tenantId: id, applicationId: id, environmentId: id }),
      revokeAllSessions: async () => { revocations++; return { revokedSessions: 3 }; },
    }, relay: async (_method, path) => {
      if (path.includes("/welcomes")) return { welcomes: [] };
      if (path.includes("/devices")) return { devices: [{ deviceId: id, status: "ACTIVE" }] };
      return { conversations: [], nextCursor: null };
    } },
    "./crypto-client": { CryptoClient: class {}, CryptoWorkerError: class extends Error {} },
  }, { process: { env: { NEXT_PUBLIC_MESSAGING_URL: "https://messaging.example" } },
    setInterval, clearInterval, setTimeout, navigator: { onLine: true },
    window: { addEventListener() {}, removeEventListener() {} }, crypto: globalThis.crypto });
  const engine = new engineModule.MessagingEngine();
  try {
    await engine.start(() => fakeCrypto);
    assert.equal(engine.getSnapshot().phase, "ready");
    assert.equal(opened, 1);
    assert.equal(await engine.revokeAllSessions(), 3);
    assert.equal(revocations, 1);
    assert.equal(closed, 1);
    assert.equal(engine.getSnapshot().sessionsPaused, true);
    assert.equal(engine.getSnapshot().deviceId, id);
    assert.deepEqual(cryptoCalls, ["open"]);
    engine.resumeSession();
    assert.equal(engine.getSnapshot().sessionsPaused, false);
    assert.equal(opened, 2);
  } finally { engine.stop(); }
});
