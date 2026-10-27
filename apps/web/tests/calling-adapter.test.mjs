import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const principalId = "11111111-1111-4111-8111-111111111111";
const endpointId = "22222222-2222-4222-8222-222222222222";
const callId = "33333333-3333-4333-8333-333333333333";
const sessionId = "44444444-4444-4444-8444-444444444444";
const identity = { principalId, tenantId: "tenant", applicationId: "app", environmentId: "env" };

// Compile the real handlers in isolation, injecting only the network and server-only
// marker. This exercises authorization decisions rather than copying their rules.
function fixture({ own = true, configured = true, scope = identity, login = true, ended = false, initiator = principalId } = {}) {
  const sent = [];
  const env = { NEXT_PUBLIC_API_URL: "https://meeting.example/api/v1", CALLING_API_URL: "https://calling.example",
    CALLING_PLATFORM_CREDENTIAL: configured ? "server-secret" : "", CALLING_TENANT_ID: "tenant", CALLING_APPLICATION_ID: "app", CALLING_ENVIRONMENT_ID: "env" };
  const call = { callId, initiatorPrincipalId: initiator,
    participants: [{ participantId: "participant", principalId: own ? principalId : "other", endedAt: ended ? "2026-10-23T12:00:00Z" : null }],
    sessions: [{ sessionId, participantId: "participant", endpointId }], invitations: [] };
  const fetch = async (url, init) => {
    sent.push({ url, init });
    if (url.endsWith("/messaging/me")) return Response.json(scope, { status: login ? 200 : 401 });
    if (url.includes("/calling/endpoints?")) return Response.json({ data: [{ endpointId, principalId, status: "ACTIVE" }] });
    if (url.endsWith(`/calls/${callId}`)) return Response.json(call);
    return Response.json({ ok: true }, { headers: { "Idempotent-Replayed": "true", "Retry-After": "7" } });
  };
  const load = (path, dependencies = {}) => {
    const exports = {};
    const js = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    vm.runInNewContext(js, { exports, require: (name) => {
      if (name === "server-only") return {};
      if (dependencies[name]) return dependencies[name];
      throw new Error(`Unexpected dependency ${name}`);
    }, process: { env }, fetch, Response, Request, Headers, AbortSignal, URL });
    return exports;
  };
  const server = load("../src/lib/calling/server.ts");
  const route = load("../src/app/api/calling/[...path]/route.ts", { "@/lib/calling/server": server });
  const signaling = load("../src/app/api/calling-signaling/[...path]/route.ts", { "@/lib/calling/server": server });
  const request = async (path, method = "GET", body, headers = {}, endpoint = false) => (endpoint ? signaling : route)[method](new Request(`https://web.example/api/calling/${path}`, {
    method, headers: { Authorization: "Bearer login-token", ...headers }, body: body ? JSON.stringify(body) : undefined,
  }), { params: Promise.resolve({ path: path.split("?")[0].split("/") }) });
  return { request, sent };
}

test("login bearer reaches only identity authority; platform secret stays on server", async () => {
  const f = fixture();
  const result = await f.request("v1/calls?principalId=attacker");
  assert.equal(result.status, 200);
  assert.equal(f.sent[0].init.headers.Authorization, "Bearer login-token");
  assert.equal(f.sent[1].init.headers.get("Authorization"), "Bearer server-secret");
  assert.equal(new URL(f.sent[1].url).searchParams.get("principalId"), principalId);
  assert.equal(result.headers.get("Cache-Control"), "no-store");
  assert.ok(!(await result.text()).includes("server-secret"));
});

test("unconfigured, unauthenticated, and foreign-scope users cannot reach Calling", async () => {
  for (const options of [{ configured: false }, { login: false }, { scope: { ...identity, tenantId: "foreign" } }]) {
    const f = fixture(options);
    const response = await f.request("v1/calls");
    assert.ok([401, 503].includes(response.status));
    assert.equal(f.sent.length, 1);
  }
});

test("foreign calls and arbitrary admin/internal paths are refused", async () => {
  const foreign = fixture({ own: false });
  assert.equal((await foreign.request(`v1/calls/${callId}/end`, "POST", { expectedVersion: 2 })).status, 404);
  assert.equal(foreign.sent.length, 2);
  for (const path of ["internal/v1/telephony/events", "v1/enterprise/members", "v1/calls/../../webhooks"]) {
    const f = fixture();
    assert.equal((await f.request(path)).status, 404);
    assert.equal(f.sent.length, 1);
  }
});

test("creation binds initiator and device, preserving version and idempotency headers", async () => {
  const f = fixture();
  const response = await f.request("v1/calls", "POST", { initiatorPrincipalId: "attacker", initiatorEndpointId: endpointId, kind: "VOICE", targetPrincipalIds: ["target"] }, { "Idempotency-Key": "stable-key" });
  assert.equal(response.status, 200);
  const forwarded = f.sent.at(-1);
  assert.equal(JSON.parse(forwarded.init.body).initiatorPrincipalId, principalId);
  assert.equal(forwarded.init.headers.get("Idempotency-Key"), "stable-key");
  assert.equal(response.headers.get("Idempotent-Replayed"), "true");
  assert.equal(response.headers.get("Retry-After"), "7");
  const invalid = fixture();
  assert.equal((await invalid.request("v1/calls", "POST", { initiatorEndpointId: "foreign" })).status, 404);
  assert.ok(!invalid.sent.some((s) => s.init.method === "POST"));
});

test("media credentials are issued only for the user's own session", async () => {
  const f = fixture();
  assert.equal((await f.request(`v1/calls/${callId}/sessions/${sessionId}/media-credentials`, "POST", {})).status, 200);
  const invalid = fixture();
  assert.equal((await invalid.request(`v1/calls/${callId}/sessions/${endpointId}/media-credentials`, "POST", {})).status, 404);
  assert.ok(!invalid.sent.some((s) => s.init.method === "POST"));
});

test("recipient cannot control the whole call but can leave their session", async () => {
  const f = fixture({ initiator: "other" });
  assert.equal((await f.request(`v1/calls/${callId}/end`, "POST", { expectedVersion: 2 })).status, 404);
  assert.equal((await f.request(`v1/calls/${callId}/sessions/${sessionId}/remove`, "POST", { expectedVersion: 2 })).status, 200);
});

test("ended participants can replay termination with their original command identity", async () => {
  const f = fixture({ ended: true });
  const response = await f.request(`v1/calls/${callId}/end`, "POST", { expectedVersion: 2 }, { "Idempotency-Key": "original-end" });
  assert.equal(response.status, 200);
  assert.equal(f.sent.at(-1).init.headers.get("Idempotency-Key"), "original-end");
  assert.equal(JSON.parse(f.sent.at(-1).init.body).expectedVersion, 2);
});

test("signaling forwards endpoint credentials alone and refuses login/admin requests", async () => {
  const f = fixture();
  const response = await f.request("v1/calling/invitations", "GET", undefined, { Authorization: "Bearer cfsig_test.token" }, true);
  assert.equal(response.status, 200);
  assert.equal(f.sent.length, 1);
  assert.equal(f.sent[0].init.headers.get("Authorization"), "Bearer cfsig_test.token");
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  const invalid = fixture();
  assert.equal((await invalid.request("v1/calling/invitations", "GET", undefined, {}, true)).status, 401);
  assert.equal((await invalid.request("v1/calls", "GET", undefined, { Authorization: "Bearer cfsig_test.token" }, true)).status, 404);
  assert.equal(invalid.sent.length, 0);
});
