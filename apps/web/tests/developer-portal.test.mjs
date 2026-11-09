// Developer Portal (Blueprint v1.0 Section 17 / ORG-017): contract pinning, session-only
// authentication, role mirror of services/developer-platform/src/domain/rbac.rs, one-time
// secret presentation and navigation rules.
//
// Run: node --experimental-strip-types --test tests/developer-portal.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  buildRequest,
  createDeveloperClient,
  DEVELOPER_OPERATIONS,
  DeveloperApiError,
  newIdempotencyKey,
  parseError,
} from "../src/lib/developer-platform/client.ts";
import { resolveDeveloperApiConfig } from "../src/lib/developer-platform/config.ts";
import { actionsFor, can, memberActions, ROLE_MATRIX, statePermits } from "../src/lib/developer-platform/permissions.ts";
import {
  canReplayDelivery,
  credentialActions,
  credentialBadges,
  credentialSecret,
  describeError,
  expiryFromLocalInput,
  invitationToken,
  limitRatio,
  safeDocumentUrl,
  shouldRetryQuery,
  validateWebhookUrl,
  webhookSecret,
} from "../src/lib/developer-platform/presentation.ts";
import { activeSection, pickWorkspace, PORTAL_SECTIONS, portalHref, safeId, visibleSections } from "../src/lib/developer-platform/navigation.ts";
import { WEBHOOK_EVENT_TYPES } from "../src/lib/developer-platform/webhook-events.ts";

const repo = (p) => fileURLToPath(new URL(`../../../${p}`, import.meta.url));
const json = (p) => JSON.parse(readFileSync(repo(p), "utf8"));
const BASE = "https://api.cfmeet.example";
const SESSION = "aaa.bbb.ccc";

// ---------------------------------------------------------------- contract pinning

test("every console operation is a published Developer Platform operation (method + path)", () => {
  const openapi = json("packages/contracts/platform-developer/http/v1.openapi.json");
  const published = {};
  for (const [path, item] of Object.entries(openapi.paths)) {
    for (const [method, op] of Object.entries(item)) {
      if (op && typeof op === "object" && op.operationId) published[op.operationId] = [method.toUpperCase(), path];
    }
  }
  for (const [id, [method, path]] of Object.entries(DEVELOPER_OPERATIONS)) {
    assert.deepEqual(published[id], [method, path], `${id} must match the contract`);
  }
});

test("webhook event picker equals the published WebhookEventType enum", () => {
  const schema = json("packages/contracts/platform-developer/webhooks/v1.schema.json");
  assert.deepEqual([...WEBHOOK_EVENT_TYPES].sort(), [...schema.$defs.WebhookEventType.enum].sort());
});

test("the portal never reaches the legacy meeting-service api-keys / webhooks API", () => {
  for (const [, path] of Object.values(DEVELOPER_OPERATIONS)) {
    assert.ok(!path.startsWith("/api/"), path);
  }
});

// ---------------------------------------------------------------- configuration

test("config: disabled unless the flag is exactly true; https only; never the legacy API", () => {
  assert.deepEqual(resolveDeveloperApiConfig(BASE, null), { available: false, reason: "DISABLED" });
  assert.deepEqual(resolveDeveloperApiConfig(BASE, "1"), { available: false, reason: "DISABLED" });
  assert.deepEqual(resolveDeveloperApiConfig(BASE, "true"), { available: true, baseUrl: BASE });
  assert.deepEqual(resolveDeveloperApiConfig(`${BASE}/`, "TRUE"), { available: true, baseUrl: BASE });
  assert.deepEqual(resolveDeveloperApiConfig("", "true"), { available: false, reason: "UNSET" });
  assert.deepEqual(resolveDeveloperApiConfig("not a url", "true"), { available: false, reason: "INVALID" });
  assert.deepEqual(resolveDeveloperApiConfig("http://api.cfmeet.example", "true"), { available: false, reason: "INSECURE" });
  assert.deepEqual(resolveDeveloperApiConfig("http://localhost:8080", "true"), { available: true, baseUrl: "http://localhost:8080" });
  assert.deepEqual(resolveDeveloperApiConfig(`${BASE}/api/v1`, "true"), { available: false, reason: "LEGACY_API" });
  assert.deepEqual(resolveDeveloperApiConfig(`https://u:p@api.cfmeet.example`, "true"), { available: false, reason: "INVALID" });
});

// ---------------------------------------------------------------- session-only auth

test("requests carry the CF Meet session bearer and refuse machine credentials", () => {
  const built = buildRequest(BASE, { operation: "listOrganizations", query: { limit: 50, cursor: null } }, SESSION);
  assert.equal(built.method, "GET");
  assert.equal(built.url, `${BASE}/v1/organizations?limit=50`);
  assert.equal(built.headers.Authorization, `Bearer ${SESSION}`);
  assert.throws(() => buildRequest(BASE, { operation: "listOrganizations" }, "cfmk_live_secret"), (e) => e.code === "MACHINE_CREDENTIAL_REFUSED");
  assert.throws(() => buildRequest(BASE, { operation: "listOrganizations" }, ""), (e) => e.status === 401);
});

test("path parameters are encoded and Idempotency-Key only goes on POST", () => {
  const built = buildRequest(
    BASE,
    { operation: "createCredential", path: { environmentId: "e/1", serviceAccountId: "s 1" }, body: { label: "x" }, idempotencyKey: newIdempotencyKey() },
    SESSION,
  );
  assert.equal(built.url, `${BASE}/v1/environments/e%2F1/service-accounts/s%201/credentials`);
  assert.match(built.headers["Idempotency-Key"], /^cfmeet-console-/);
  assert.throws(() => buildRequest(BASE, { operation: "getProject", path: { projectId: "p" }, idempotencyKey: "k" }, SESSION));
  assert.throws(() => buildRequest(BASE, { operation: "getProject", path: {} }, SESSION), /missing path parameter/);
});

test("client: unavailable when not configured, and calls nothing", async () => {
  let calls = 0;
  const client = createDeveloperClient({ baseUrl: null, getSession: () => SESSION, fetchImpl: async () => (calls++, new Response("{}")) });
  await assert.rejects(client.call({ operation: "listOrganizations" }), (e) => e.kind === "unavailable");
  assert.equal(calls, 0);
});

test("client: async session, one refresh on 401, no cookies, no redirects", async () => {
  const seen = [];
  const fetchImpl = async (url, init) => {
    seen.push({ url, init });
    if (seen.length === 1) return new Response(JSON.stringify({ error: { code: "AUTHENTICATION_REQUIRED", message: "x" } }), { status: 401 });
    return new Response(JSON.stringify({ items: [], nextCursor: null }), { status: 200, headers: { "x-request-id": "req-1" } });
  };
  const client = createDeveloperClient({
    baseUrl: BASE,
    getSession: async () => "old.session.token",
    refreshSession: async () => "new.session.token",
    fetchImpl,
  });
  const result = await client.request({ operation: "listOrganizations" });
  assert.equal(result.requestId, "req-1");
  assert.equal(seen.length, 2);
  assert.equal(seen[0].init.headers.Authorization, "Bearer old.session.token");
  assert.equal(seen[1].init.headers.Authorization, "Bearer new.session.token");
  for (const { init } of seen) {
    assert.equal(init.credentials, "omit");
    assert.equal(init.redirect, "error");
    assert.equal(init.cache, "no-store");
  }
});

test("client: a signed-out user gets a session error, not a network call", async () => {
  let calls = 0;
  const client = createDeveloperClient({
    baseUrl: BASE,
    getSession: async () => null,
    refreshSession: async () => {
      throw new Error("ended");
    },
    fetchImpl: async () => (calls++, new Response("{}")),
  });
  await assert.rejects(client.call({ operation: "listOrganizations" }), (e) => e.kind === "session" && e.status === 401);
  assert.equal(calls, 0);
});

test("client: network failure and replayed idempotent creates are surfaced", async () => {
  const down = createDeveloperClient({ baseUrl: BASE, getSession: () => SESSION, fetchImpl: async () => Promise.reject(new TypeError("fetch failed")) });
  await assert.rejects(down.call({ operation: "listOrganizations" }), (e) => e.kind === "network");
  const replay = createDeveloperClient({
    baseUrl: BASE,
    getSession: () => SESSION,
    fetchImpl: async () => new Response(JSON.stringify({ credential: { id: "c" }, secretRedacted: true }), { status: 201, headers: { "idempotent-replayed": "true" } }),
  });
  const r = await replay.request({
    operation: "createCredential",
    path: { environmentId: "e", serviceAccountId: "s" },
    body: {},
    idempotencyKey: "k-1",
  });
  assert.equal(r.replayed, true);
  assert.deepEqual(credentialSecret(r.data), { kind: "redacted" });
});

test("parseError reads the contract error body, request id and Retry-After", () => {
  const headers = new Headers({ "x-request-id": "hdr", "retry-after": "7" });
  const e = parseError(429, headers, JSON.stringify({ error: { code: "RATE_LIMITED", message: "slow", requestId: "body" } }));
  assert.ok(e instanceof DeveloperApiError);
  assert.equal(e.code, "RATE_LIMITED");
  assert.equal(e.requestId, "body");
  assert.equal(e.retryAfterSeconds, 7);
  const proxy = parseError(502, new Headers(), "<html>");
  assert.equal(proxy.code, "HTTP_502");
});

// ---------------------------------------------------------------- roles (rbac.rs mirror)

const RUST_ROLES = { Owner: "OWNER", Admin: "ADMIN", Developer: "DEVELOPER", Analyst: "ANALYST", Billing: "BILLING" };
const ALL = Object.values(RUST_ROLES);

function rustMatrix() {
  const src = readFileSync(repo("services/developer-platform/src/domain/rbac.rs"), "utf8");
  const body = src.slice(src.indexOf("pub fn allows("), src.indexOf("/// Organization lifecycle"));
  const arms = body.slice(body.indexOf("match action {") + "match action {".length).replace(/\/\/[^\n]*/g, "");
  const matrix = {};
  const re = /([A-Za-z|\s]+?)=>\s*(\{[^}]*\}|matches!\([^)]*\)|true)/g;
  let m;
  while ((m = re.exec(arms))) {
    const actions = m[1].split("|").map((s) => s.trim()).filter(Boolean);
    const rhs = m[2];
    let roles;
    if (/^\s*true\s*$/.test(rhs)) roles = ALL;
    else if (/role == (\w+)/.test(rhs)) roles = [RUST_ROLES[rhs.match(/role == (\w+)/)[1]]];
    else roles = rhs.match(/matches!\(role,([^)]*)\)/)[1].split("|").map((s) => RUST_ROLES[s.trim()]);
    for (const a of actions) matrix[a] = roles;
  }
  return matrix;
}

test("ROLE_MATRIX equals the Developer Platform authorization table", () => {
  const rust = rustMatrix();
  assert.deepEqual(Object.keys(ROLE_MATRIX).sort(), Object.keys(rust).sort());
  for (const [action, roles] of Object.entries(rust)) {
    assert.deepEqual([...ROLE_MATRIX[action]].sort(), [...roles].sort(), action);
  }
});

test("workspace state gates mutations like OrganizationState::permits", () => {
  assert.equal(statePermits("ACTIVE", "ProjectsManage"), true);
  assert.equal(statePermits("RESTRICTED", "ProjectsManage"), false);
  assert.equal(statePermits("RESTRICTED", "CredentialsRevoke"), true);
  assert.equal(statePermits("RESTRICTED", "UsageRead"), true);
  assert.equal(statePermits("SUSPENDED", "CredentialsRevoke"), false);
  assert.equal(statePermits("CLOSED", "ActivityRead"), true);
  assert.equal(statePermits("INVITED", "OrganizationRead"), false);
  assert.equal(can(null, "OrganizationRead"), false);
  assert.equal(can({ myRole: null, state: "ACTIVE" }, "OrganizationRead"), false);
});

test("developer roles are independent: an ANALYST cannot touch keys, a BILLING member sees plans", () => {
  const analyst = { myRole: "ANALYST", state: "ACTIVE" };
  assert.equal(can(analyst, "CredentialsManage"), false);
  assert.equal(can(analyst, "ServiceAccountsRead"), false);
  assert.equal(can(analyst, "ActivityRead"), true);
  const billing = { myRole: "BILLING", state: "ACTIVE" };
  assert.equal(can(billing, "CommercialRead"), true);
  assert.equal(can(billing, "WebhooksRead"), false);
  assert.ok(actionsFor("OWNER").length > actionsFor("DEVELOPER").length);
});

test("member row actions: owners manage, anyone may leave, the last owner is protected", () => {
  const owner = { myRole: "OWNER", state: "ACTIVE" };
  const dev = { myRole: "DEVELOPER", state: "ACTIVE" };
  const other = { userId: "u2", role: "DEVELOPER", status: "ACTIVE" };
  const me = { userId: "u1", role: "OWNER", status: "ACTIVE" };
  assert.deepEqual(memberActions(owner, other, "u1", 1), { changeRole: true, remove: true, leave: false, transferTo: true });
  assert.deepEqual(memberActions(owner, me, "u1", 1), { changeRole: false, remove: false, leave: false, transferTo: false });
  assert.equal(memberActions(owner, me, "u1", 2).leave, true);
  assert.deepEqual(memberActions(dev, { ...other, userId: "u1" }, "u1", 1), { changeRole: false, remove: false, leave: true, transferTo: false });
  assert.equal(memberActions(dev, other, "u1", 1).remove, false);
  assert.equal(memberActions(owner, { ...other, status: "REMOVED" }, "u1", 1).remove, false);
});

// ---------------------------------------------------------------- presentation

test("one-time secrets: shown when returned, redacted on replay", () => {
  assert.deepEqual(credentialSecret({ credential: {}, secret: "cfmk_x" }), { kind: "secret", value: "cfmk_x" });
  assert.deepEqual(credentialSecret({ credential: {}, secretRedacted: true }), { kind: "redacted" });
  assert.deepEqual(webhookSecret({ webhook: {}, signingSecret: "whsec" }), { kind: "secret", value: "whsec" });
  assert.deepEqual(webhookSecret({ webhook: {} }), { kind: "redacted" });
  assert.deepEqual(invitationToken({ token: "" }), { kind: "redacted" });
});

const credential = (over = {}) => ({
  effectiveState: "ACTIVE",
  status: "ACTIVE",
  expiringSoon: false,
  expiresAt: null,
  lastAuthenticatedAt: null,
  ...over,
});

test("credential badges show state, expiring soon, expired and last use (PLT-008)", () => {
  const now = Date.parse("2026-11-08T12:00:00Z");
  const labels = (c) => credentialBadges(c, now).map((b) => b.label);
  assert.deepEqual(labels(credential()), ["ACTIVE", "Never used"]);
  assert.deepEqual(labels(credential({ expiringSoon: true, lastAuthenticatedAt: "2026-11-08T10:30:00Z" })), [
    "ACTIVE",
    "Expiring soon",
    "Last used 2026-11-08 10:30 UTC",
  ]);
  assert.ok(labels(credential({ effectiveState: "EXPIRED", expiresAt: "2026-11-01T00:00:00Z" })).includes("Expired"));
  assert.equal(credentialBadges(credential({ effectiveState: "ROTATION_OVERLAP" }), now)[0].tone, "warn");
  assert.equal(credentialBadges(credential({ effectiveState: "REVOKED", status: "REVOKED" }), now)[0].tone, "bad");
});

test("credential actions follow the effective state", () => {
  assert.deepEqual(credentialActions(credential()), { rotate: true, suspend: true, resume: false, revoke: true });
  assert.deepEqual(credentialActions(credential({ effectiveState: "SUSPENDED" })), { rotate: false, suspend: false, resume: true, revoke: true });
  assert.deepEqual(credentialActions(credential({ effectiveState: "SUPERSEDED" })), { rotate: false, suspend: false, resume: false, revoke: true });
  assert.deepEqual(credentialActions(credential({ status: "REVOKED", effectiveState: "REVOKED" })), { rotate: false, suspend: false, resume: false, revoke: false });
});

test("error presentation: contract codes win over the status family", () => {
  assert.equal(describeError({ status: 404, code: "INVITATION_INVALID" }).detail.includes("invalid"), true);
  assert.equal(describeError({ status: 403, code: "INVITATION_EMAIL_MISMATCH" }).detail.includes("different email"), true);
  assert.equal(describeError({ status: 403, code: "ORGANIZATION_RESTRICTED" }).title, "Workspace state");
  assert.equal(describeError({ status: 403, code: "PERMISSION_DENIED" }).title, "Not allowed");
  assert.equal(describeError({ status: 409, code: "VERSION_CONFLICT" }).action, "reload");
  assert.equal(describeError({ status: 401, code: "AUTHENTICATION_REQUIRED" }).action, "signin");
  assert.equal(describeError({ kind: "unavailable", message: "m" }).title, "Developer Platform not available in this environment");
  assert.equal(describeError({ kind: "network" }).action, "retry");
  assert.equal(describeError({ kind: "session", code: "SESSION_UNAVAILABLE", status: 0 }).action, "retry");
  const limited = describeError({ status: 429, retryAfterSeconds: 3, requestId: "r" });
  assert.match(limited.detail, /3 s/);
  assert.equal(limited.requestId, "r");
});

test("reads retry only transient failures", () => {
  assert.equal(shouldRetryQuery(0, { kind: "network" }), true);
  assert.equal(shouldRetryQuery(0, { status: 503 }), true);
  assert.equal(shouldRetryQuery(0, { status: 403 }), false);
  assert.equal(shouldRetryQuery(2, { status: 503 }), false);
});

test("form validation: webhook URL, expiry, limits, doc links", () => {
  assert.equal(validateWebhookUrl("https://hooks.example.com/x"), null);
  assert.match(validateWebhookUrl("http://hooks.example.com/x"), /HTTPS/);
  assert.equal(validateWebhookUrl("http://localhost:9000/x"), null);
  assert.match(validateWebhookUrl("https://u:p@hooks.example.com"), /credentials/);
  assert.match(validateWebhookUrl("javascript:alert(1)"), /HTTPS/);
  const now = Date.parse("2026-11-08T12:00:00Z");
  assert.deepEqual(expiryFromLocalInput("", now), { ok: true, iso: null });
  assert.equal(expiryFromLocalInput("2026-11-01T00:00", now).ok, false);
  assert.equal(expiryFromLocalInput("2027-01-01T00:00Z", now).ok, true);
  assert.equal(limitRatio(50, 100), 0.5);
  assert.equal(limitRatio(150, 100), 1);
  assert.equal(limitRatio(null, 100), null);
  assert.equal(safeDocumentUrl("/openapi.json", BASE), `${BASE}/openapi.json`);
  assert.equal(safeDocumentUrl("javascript:alert(1)", BASE), null);
  assert.equal(safeDocumentUrl("http://evil.example/x", BASE), null);
});

test("replay needs an ACTIVE subscription and a settled delivery", () => {
  assert.equal(canReplayDelivery("ACTIVE", { status: "FAILED" }), true);
  assert.equal(canReplayDelivery("DISABLED", { status: "FAILED" }), false);
  assert.equal(canReplayDelivery("ACTIVE", { status: "DELIVERING" }), false);
});

// ---------------------------------------------------------------- navigation

test("sidebar shows only the sections the developer role can use", () => {
  const keys = (ws) => visibleSections((a) => can(ws, a)).map((s) => s.key);
  assert.deepEqual(keys(null), ["overview", "workspace", "docs"]);
  assert.deepEqual(keys({ myRole: "OWNER", state: "ACTIVE" }), PORTAL_SECTIONS.map((s) => s.key));
  assert.ok(!keys({ myRole: "BILLING", state: "ACTIVE" }).includes("credentials"));
  assert.ok(!keys({ myRole: "DEVELOPER", state: "ACTIVE" }).includes("audit"));
  assert.ok(keys({ myRole: "ANALYST", state: "ACTIVE" }).includes("audit"));
});

test("active section, workspace pick and URL ids", () => {
  assert.equal(activeSection("/developers"), "overview");
  assert.equal(activeSection("/developers/credentials"), "credentials");
  assert.equal(activeSection("/developers/projects/"), "projects");
  assert.equal(activeSection("/dashboard"), null);
  const ws = [
    { id: "a", state: "CLOSED" },
    { id: "b", state: "ACTIVE" },
  ];
  assert.equal(pickWorkspace(ws, "a").id, "a");
  assert.equal(pickWorkspace(ws, "gone").id, "b");
  assert.equal(pickWorkspace(ws, "<script>").id, "b");
  assert.equal(pickWorkspace([], null), null);
  assert.equal(safeId("2b9c-11"), "2b9c-11");
  assert.equal(safeId("../x"), null);
  assert.equal(portalHref("/developers/usage", { project: "p", env: null }), "/developers/usage?project=p");
});

// ---------------------------------------------------------------- no secrets at rest

test("portal source never persists secrets or logs them", () => {
  const files = [
    "apps/web/src/components/developer-portal/ui.tsx",
    "apps/web/src/components/developer-portal/DeveloperPortalProvider.tsx",
    "apps/web/src/components/developer-portal/pages/CredentialsPage.tsx",
    "apps/web/src/components/developer-portal/pages/WebhooksPage.tsx",
    "apps/web/src/components/developer-portal/pages/MembersPage.tsx",
    "apps/web/src/lib/developer-platform/client.ts",
  ];
  for (const f of files) {
    const src = readFileSync(repo(f), "utf8");
    assert.ok(!/console\.(log|info|debug|warn|error)/.test(src), `${f} must not log`);
    assert.ok(!/sessionStorage|indexedDB/.test(src), `${f} must not persist`);
    for (const m of src.matchAll(/localStorage\.setItem\(([^)]*)\)/g)) {
      assert.match(m[1], /SELECTED_WORKSPACE_KEY/, `${f}: only the selected workspace id may be stored`);
    }
  }
});
