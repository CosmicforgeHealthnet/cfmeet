import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const principalId = "11111111-1111-4111-8111-111111111111";
const endpointId = "22222222-2222-4222-8222-222222222222";
const callId = "33333333-3333-4333-8333-333333333333";
const sessionId = "44444444-4444-4444-8444-444444444444";
const blockId = "55555555-5555-4555-8555-555555555555";
const identity = { principalId, tenantId: "tenant", applicationId: "app", environmentId: "env" };

// Compile the real handlers in isolation, injecting only the network and server-only
// marker. This exercises authorization decisions rather than copying their rules.
function fixture({ own = true, ownsBlock = true, configured = true, scope = identity, login = true, ended = false, initiator = principalId, admin = false, state = "CONNECTED", extraEnv = {}, upstream = () => null } = {}) {
  const sent = [];
  const env = { NEXT_PUBLIC_API_URL: "https://meeting.example/api/v1", CALLING_API_URL: "https://calling.example",
    CALLING_PLATFORM_CREDENTIAL: configured ? "server-secret" : "", CALLING_TENANT_ID: "tenant", CALLING_APPLICATION_ID: "app", CALLING_ENVIRONMENT_ID: "env",
    CALLING_ADMIN_PRINCIPAL_IDS: admin ? principalId : "", CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS: admin ? principalId : "", ...extraEnv };
  const call = { callId, state: ended ? "ENDED" : state, initiatorPrincipalId: initiator,
    participants: [{ participantId: "participant", principalId: own ? principalId : "other", role: initiator === principalId ? "INITIATOR" : "RECIPIENT", endedAt: ended ? "2026-10-23T12:00:00Z" : null },
      { participantId: "peer", principalId: "other-party", role: initiator === principalId ? "RECIPIENT" : "INITIATOR", endedAt: null }],
    sessions: [{ sessionId, participantId: "participant", endpointId }], invitations: [] };
  const fetch = async (url, init) => {
    sent.push({ url, init });
    const custom = upstream(url, init);
    if (custom) return custom;
    if (url.endsWith("/messaging/me")) return Response.json(scope, { status: login ? 200 : 401 });
    if (url.includes("/calling/endpoints?")) return Response.json({ data: [{ endpointId, principalId, status: "ACTIVE" }] });
    if (url.includes("/v1/blocks?principalId=")) return Response.json({ items: ownsBlock ? [{ blockId, principalId }] : [] });
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
  const guard = load("../src/lib/calling/runtime-guard.ts");
  const permissions = load("../src/lib/calling/call-permissions.ts");
  const server = load("../src/lib/calling/server.ts", { "@/lib/calling/runtime-guard": guard });
  const route = load("../src/app/api/calling/[...path]/route.ts", { "@/lib/calling/server": server, "@/lib/calling/call-permissions": permissions });
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

test("public enterprise and Privacy Relay routes are allowlisted by contract method", async () => {
  const f = fixture({ admin: true });
  for (const [path, method, body] of [
    ["v1/enterprise/extensions", "GET"],
    ["v1/calling/usage", "GET"],
    ["v1/calling/limits", "GET"],
    ["v1/enterprise/extensions", "POST", { extension: "4100" }],
    ["v1/enterprise/departments/55555555-5555-4555-8555-555555555555/members", "PUT", { members: [] }],
    ["v1/privacy-relay/contexts", "GET"],
    ["v1/privacy-relay/contexts", "POST", { participants: [] }],
    ["v1/privacy-relay/contexts/66666666-6666-4666-8666-666666666666", "PATCH", { status: "SUSPENDED" }],
    ["v1/privacy-relay/contexts/66666666-6666-4666-8666-666666666666/sessions", "POST", { channel: "VOICE" }],
  ]) {
    assert.equal((await f.request(path, method, body)).status, 200, `${method} ${path}`);
  }
  assert.equal(f.sent.length, 18);
  assert.equal(JSON.parse(f.sent.at(-1).init.body).channel, "VOICE");

  for (const [path, variable] of [
    ["v1/enterprise/extensions", "CALLING_ADMIN_PRINCIPAL_IDS"],
    ["v1/calling/usage", "CALLING_ADMIN_PRINCIPAL_IDS"],
    ["v1/privacy-relay/contexts", "CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS"],
  ]) {
    const denied = fixture();
    const response = await denied.request(path);
    assert.equal(response.status, 403, `${path} requires ${variable}`);
    assert.equal(denied.sent.length, 1, "an unauthorized resource request must not reach Calling");
  }

  for (const [path, method] of [
    ["v1/enterprise/extensions", "DELETE"],
    ["v1/calling/usage", "POST"],
    ["v1/enterprise/effective-policy", "POST"],
    ["v1/privacy-relay/contexts/66666666-6666-4666-8666-666666666666/evidence", "POST"],
    ["v1/privacy-relay/contexts/not-a-uuid", "GET"],
    ["internal/v1/privacy-relay/enforcement", "POST"],
  ]) {
    const denied = fixture();
    assert.equal((await denied.request(path, method, method === "GET" ? undefined : {})).status, 404, `${method} ${path}`);
    assert.equal(denied.sent.length, 1, "denied paths must not reach Calling");
  }
});

test("block rules are bound to the signed-in principal and deletes verify ownership", async () => {
  const create = fixture();
  const created = await create.request("v1/blocks", "POST", {
    principalId: "attacker",
    blockedPrincipalId: "77777777-7777-4777-8777-777777777777",
    reasonCode: "UNWANTED",
  });
  assert.equal(created.status, 200);
  assert.equal(JSON.parse(create.sent.at(-1).init.body).principalId, principalId);

  const list = fixture();
  assert.equal((await list.request("v1/blocks?principalId=attacker")).status, 200);
  assert.equal(new URL(list.sent.at(-1).url).searchParams.get("principalId"), principalId);

  const remove = fixture();
  assert.equal((await remove.request(`v1/blocks/${blockId}`, "DELETE")).status, 200);
  assert.equal(new URL(remove.sent.at(-2).url).searchParams.get("principalId"), principalId);
  assert.equal(remove.sent.at(-1).url.endsWith(`/v1/blocks/${blockId}`), true);

  const foreign = fixture({ ownsBlock: false });
  assert.equal((await foreign.request(`v1/blocks/${blockId}`, "DELETE")).status, 404);
  assert.equal(foreign.sent.some((item) => item.init.method === "DELETE"), false);
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

test("any active party may end, hold and resume; only the caller cancels (CAL-002, ADR 0009)", async () => {
  const recipient = fixture({ initiator: "other-party" });
  for (const op of ["end", "hold", "resume"]) {
    assert.equal((await recipient.request(`v1/calls/${callId}/${op}`, "POST", { expectedVersion: 2 })).status, 200, op);
  }
  assert.equal((await recipient.request(`v1/calls/${callId}/sessions/${sessionId}/remove`, "POST", { expectedVersion: 2 })).status, 200);
  const ringing = fixture({ initiator: "other-party", state: "RINGING" });
  assert.equal((await ringing.request(`v1/calls/${callId}/cancel`, "POST", { expectedVersion: 2 })).status, 404);
  const caller = fixture({ state: "RINGING" });
  assert.equal((await caller.request(`v1/calls/${callId}/cancel`, "POST", { expectedVersion: 2 })).status, 200);
  // A transfer target (non-initiator) can still end a transferred call.
  const transferred = fixture({ initiator: "retired-transferor" });
  assert.equal((await transferred.request(`v1/calls/${callId}/end`, "POST", { expectedVersion: 3 })).status, 200);
  const transfer = fixture({ initiator: "other-party" });
  assert.equal((await transfer.request(`v1/calls/${callId}/transfer`, "POST", { transferringParticipantId: "peer", targetExtension: "4100", expectedVersion: 2 })).status, 200);
  assert.equal(JSON.parse(transfer.sent.at(-1).init.body).transferringParticipantId, "participant");
});

test("a party that left a live group call can no longer control it", async () => {
  const left = fixture({ upstream: (url) => url.endsWith(`/calls/${callId}`) ? Response.json({ callId, state: "CONNECTED", initiatorPrincipalId: "other-party",
    participants: [{ participantId: "participant", principalId, endedAt: "2026-10-23T12:00:00Z" }, { participantId: "a", principalId: "x", endedAt: null }, { participantId: "b", principalId: "y", endedAt: null }],
    sessions: [], invitations: [] }) : null });
  assert.equal((await left.request(`v1/calls/${callId}/end`, "POST", { expectedVersion: 2 })).status, 404);
  assert.equal((await left.request(`v1/calls/${callId}`)).status, 200);
});

test("recording consent and controls are bound to the participant and its own endpoint (CAL-005)", async () => {
  const f = fixture();
  assert.equal((await f.request(`v1/calls/${callId}/recording-authorizations?principalId=attacker`)).status, 200);
  assert.equal(new URL(f.sent.at(-1).url).searchParams.get("principalId"), principalId);
  assert.equal((await f.request(`v1/calls/${callId}/recording-consent`, "POST", { endpointId, decision: "GRANTED", participantId: "forged" })).status, 200);
  assert.deepEqual(JSON.parse(f.sent.at(-1).init.body), { endpointId, decision: "GRANTED" });
  assert.equal((await f.request(`v1/calls/${callId}/recordings`, "POST", { principalId: "attacker" })).status, 200);
  assert.deepEqual(JSON.parse(f.sent.at(-1).init.body), { principalId });
  assert.equal((await f.request(`v1/calls/${callId}/recordings/${blockId}/stop`, "POST", {})).status, 200);
  assert.deepEqual(JSON.parse(f.sent.at(-1).init.body), { principalId });
  const invalid = fixture();
  assert.equal((await invalid.request(`v1/calls/${callId}/recording-consent`, "POST", { endpointId: "foreign", decision: "GRANTED" })).status, 404);
  assert.equal((await invalid.request(`v1/calls/${callId}/recording-consent`, "POST", { endpointId, decision: "MAYBE" })).status, 404);
  assert.ok(!invalid.sent.some((s) => s.url.includes("recording-consent")));
  const foreign = fixture({ own: false });
  assert.equal((await foreign.request(`v1/calls/${callId}/recordings`, "POST", {})).status, 404);
});

test("abuse reports carry the reporter and a bounded category only (CAL-005)", async () => {
  const f = fixture({ ended: true });
  assert.equal((await f.request(`v1/calls/${callId}/abuse-reports`, "POST", { principalId: "attacker", category: "SPAM", description: "free text" })).status, 200);
  assert.deepEqual(JSON.parse(f.sent.at(-1).init.body), { principalId, category: "SPAM" });
  assert.equal((await f.request(`v1/calls/${callId}/abuse-reports`, "POST", { category: "SCAM" })).status, 400);
  assert.equal((await f.request("v1/abuse-reports?principalId=attacker")).status, 200);
  assert.equal(new URL(f.sent.at(-1).url).searchParams.get("principalId"), principalId);
});

test("voicemail is self-service: owner view only, no allowlist (CAL-006)", async () => {
  const mailbox = "66666666-6666-4666-8666-666666666666";
  const shared = "77777777-7777-4777-8777-777777777777";
  const foreign = "88888888-8888-4888-8888-888888888888";
  const f = fixture({ upstream: (url) => {
    if (url.includes("/voicemail/mailboxes?")) return Response.json({ mailboxes: [
      { mailboxId: mailbox, owner: { type: "PRINCIPAL", id: principalId } },
      { mailboxId: shared, owner: { type: "EXTENSION", id: blockId } },
      { mailboxId: foreign, owner: { type: "PRINCIPAL", id: "someone-else" } }], nextCursor: null });
    if (url.includes(`messages?mailboxId=${shared}`)) return Response.json({ messages: [], nextCursor: null });
    return null;
  } });
  const list = await f.request("v1/enterprise/voicemail/mailboxes");
  assert.equal(list.status, 200);
  assert.deepEqual((await list.json()).mailboxes.map((m) => m.mailboxId), [mailbox, shared]);
  assert.equal((await f.request(`v1/enterprise/voicemail/messages?mailboxId=${mailbox}`)).status, 200);
  assert.equal(new URL(f.sent.at(-1).url).searchParams.get("principalId"), principalId);
  assert.equal((await f.request(`v1/enterprise/voicemail/messages/${mailbox}/media-access`, "POST", {})).status, 200);
  assert.deepEqual(JSON.parse(f.sent.at(-1).init.body), { principalId });
  assert.equal((await f.request(`v1/enterprise/voicemail/messages/${mailbox}/read-state`, "PUT", { expectedVersion: 1, readState: "READ", principalId: "attacker" })).status, 200);
  assert.equal(JSON.parse(f.sent.at(-1).init.body).principalId, principalId);
  assert.equal((await f.request(`v1/enterprise/voicemail/messages/${mailbox}?expectedVersion=2`, "DELETE")).status, 200);
  assert.equal(new URL(f.sent.at(-1).url).searchParams.get("principalId"), principalId);
  // Mailbox administration of the shared scope stays allowlisted.
  const admin = fixture();
  assert.equal((await admin.request("v1/enterprise/voicemail/mailboxes", "POST", { displayLabel: "x" })).status, 403);
});

test("forwarding rules are the user's own and go to people, extensions or voicemail only (CAL-006)", async () => {
  const rule = "66666666-6666-4666-8666-666666666666";
  const target = "77777777-7777-4777-8777-777777777777";
  const f = fixture({ upstream: (url, init) => url.endsWith(`/forwarding-rules/${rule}`) && (!init?.method || init.method === "GET") ? Response.json({ forwardingRuleId: rule, principalId }) : null });
  assert.equal((await f.request("v1/enterprise/forwarding-rules?principalId=attacker")).status, 200);
  assert.equal(new URL(f.sent.at(-1).url).searchParams.get("principalId"), principalId);
  assert.equal((await f.request("v1/enterprise/forwarding-rules", "POST", { principalId: "attacker", condition: "NO_ANSWER", destination: { type: "PRINCIPAL", id: target } })).status, 200);
  assert.equal(JSON.parse(f.sent.at(-1).init.body).principalId, principalId);
  assert.equal((await f.request("v1/enterprise/forwarding-rules", "POST", { condition: "ALWAYS", destination: { type: "QUEUE", id: target } })).status, 403);
  assert.equal((await f.request(`v1/enterprise/forwarding-rules/${rule}?expectedVersion=1`, "DELETE")).status, 200);
  const other = fixture({ upstream: (url) => url.endsWith(`/forwarding-rules/${rule}`) ? Response.json({ forwardingRuleId: rule, principalId: "someone-else" }) : null });
  assert.equal((await other.request(`v1/enterprise/forwarding-rules/${rule}?expectedVersion=1`, "DELETE")).status, 404);
  assert.ok(!other.sent.some((s) => s.init?.method === "DELETE"));
});

test("CALLING_DEV_MODE never disables authentication in a production build or with a credential (CAL-008)", async () => {
  const production = fixture({ extraEnv: { CALLING_DEV_MODE: "true", NODE_ENV: "production" } });
  const response = await production.request("v1/calls");
  assert.equal(response.status, 503);
  assert.equal((await response.json()).error.code, "CALLING_NOT_CONFIGURED");
  assert.equal(production.sent.length, 0);
  const withCredential = fixture({ extraEnv: { CALLING_DEV_MODE: "true" } });
  assert.equal((await withCredential.request("v1/calls")).status, 503);
  const dev = fixture({ configured: false, extraEnv: { CALLING_DEV_MODE: "true", NODE_ENV: "development" } });
  assert.equal((await dev.request("v1/calls")).status, 200);
  assert.equal(dev.sent.length, 0);
});

test("production refuses a plain-http Calling origin for the adapter and the signaling transport (CAL-020)", async () => {
  const f = fixture({ extraEnv: { NODE_ENV: "production", CALLING_API_URL: "http://calling.example" } });
  assert.equal((await f.request("v1/calls")).status, 503);
  assert.equal((await f.request("v1/calling/invitations", "GET", undefined, { Authorization: "Bearer cfsig_test.token" }, true)).status, 503);
  assert.ok(!f.sent.some((s) => s.url.startsWith("http://calling.example")));
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
