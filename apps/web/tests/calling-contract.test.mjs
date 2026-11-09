// CAL-022: web calling is typed against the SERVED contracts, not invented endpoints.
//
// Every path the web calling clients call must exist (with that method) in:
//   packages/contracts/calling/http/v1.openapi.json           (Calling, via the same-origin adapter)
//   packages/contracts/communications/http/v1.openapi.json    (Hub call launch / contacts, via Meeting)
//   packages/contracts/openapi/meeting-service.json           (organization calling relay, principal names)
// and the hand-written mirror types must carry exactly the contract's enum values.
// Run: node --test tests/calling-contract.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, readdirSync } from "node:fs";

const root = new URL("../../../", import.meta.url);
const web = new URL("../", import.meta.url);
const json = (path) => JSON.parse(readFileSync(new URL(path, root), "utf8"));
const source = (path) => readFileSync(new URL(path, web), "utf8");

const calling = json("packages/contracts/calling/http/v1.openapi.json");
const enterprise = json("packages/contracts/calling/enterprise/v1.schema.json").$defs;
const common = json("packages/contracts/calling/common/v1.schema.json").$defs;
const control = json("packages/contracts/calling/control/v1.schema.json").$defs;
const hub = json("packages/contracts/communications/http/v1.openapi.json");
const meeting = json("packages/contracts/openapi/meeting-service.json");

const METHODS = ["get", "post", "put", "patch", "delete"];
/** "/v1/calls/{call_id}/end" -> regex matching "/v1/calls/{x}/end" with any parameter name. */
function operations(spec) {
  const out = [];
  for (const [path, item] of Object.entries(spec.paths)) {
    for (const method of METHODS) {
      if (item[method]) out.push({ method: method.toUpperCase(), path, shape: path.replace(/\{[^}]+\}/g, "{}") });
    }
  }
  return out;
}
const callingOps = operations(calling);
const hubOps = operations(hub);
const meetingOps = operations(meeting);
const has = (ops, method, path) => ops.some((o) => o.shape === path.replace(/\{[^}]+\}/g, "{}") && (!method || o.method === method));

/** Literal request paths of a client file: `${expr}` becomes `{}`; helper prefixes are expanded. */
function requestedPaths(text, helpers = {}) {
  let t = text;
  for (const [helper, expansion] of Object.entries(helpers)) t = t.replaceAll(helper, expansion);
  const found = [];
  const call = /\.(get|post|put|patch|delete)(?:<[^()]*?>)?\(\s*(`[^`]*`|"[^"]*")/g;
  for (const m of t.matchAll(call)) found.push({ method: m[1].toUpperCase(), path: m[2].slice(1, -1).replace(/\$\{[^}]*\}/g, "{}").split("?")[0] });
  const literal = /(`|")(\/(?:v1|communications|organizations|messaging)\/[^`"]*)\1/g;
  for (const m of t.matchAll(literal)) found.push({ method: null, path: m[2].replace(/\$\{[^}]*\}/g, "{}").split("?")[0] });
  return found;
}

test("every Calling path the web services call exists in the served Calling contract with that method", () => {
  const dir = new URL("src/services/calling/", web);
  const files = readdirSync(dir).filter((f) => f.endsWith(".service.ts") && !["hub-calls.service.ts", "organization-calling.service.ts"].includes(f));
  let checked = 0;
  for (const file of files) {
    const paths = requestedPaths(source(`src/services/calling/${file}`), { "${call(callId)}": "/v1/calls/${callId}" })
      .filter((p) => p.path.startsWith("/v1/"));
    for (const { method, path } of paths) {
      assert.ok(has(callingOps, method, path), `${file}: ${method ?? "ANY"} ${path} is not in the Calling contract`);
      checked += 1;
    }
  }
  assert.ok(checked >= 30, `expected to check the calling client surface, checked ${checked}`);
});

test("the adapter's participant-bound and self-service operations exist in the Calling contract", () => {
  for (const [method, path] of [
    ["POST", "/v1/calls/{id}/end"], ["POST", "/v1/calls/{id}/hold"], ["POST", "/v1/calls/{id}/resume"], ["POST", "/v1/calls/{id}/cancel"],
    ["POST", "/v1/calls/{id}/transfer"], ["POST", "/v1/calls/{id}/sessions/{id}/remove"],
    ["GET", "/v1/calls/{id}/recording-authorizations"], ["POST", "/v1/calls/{id}/recording-consent"],
    ["GET", "/v1/calls/{id}/recordings"], ["POST", "/v1/calls/{id}/recordings"], ["POST", "/v1/calls/{id}/recordings/{id}/stop"],
    ["POST", "/v1/calls/{id}/abuse-reports"], ["GET", "/v1/abuse-reports"],
    ["GET", "/v1/enterprise/voicemail/mailboxes"], ["GET", "/v1/enterprise/voicemail/messages"],
    ["GET", "/v1/enterprise/voicemail/messages/{id}"], ["DELETE", "/v1/enterprise/voicemail/messages/{id}"],
    ["PUT", "/v1/enterprise/voicemail/messages/{id}/read-state"], ["POST", "/v1/enterprise/voicemail/messages/{id}/media-access"],
    ["GET", "/v1/enterprise/forwarding-rules"], ["POST", "/v1/enterprise/forwarding-rules"],
    ["GET", "/v1/enterprise/forwarding-rules/{id}"], ["PATCH", "/v1/enterprise/forwarding-rules/{id}"], ["DELETE", "/v1/enterprise/forwarding-rules/{id}"],
    ["GET", "/v1/calling/invitations"], ["POST", "/v1/calling/invitations/{id}/accept"], ["POST", "/v1/calling/sessions/{id}/media-credentials"],
  ]) assert.ok(has(callingOps, method, path), `${method} ${path}`);
  // The owner view is reached with principalId (VoicemailRequester::Principal), which the contract documents.
  const params = (path, method) => (calling.paths[path][method].parameters ?? []).map((p) => p.name);
  assert.ok(params("/v1/enterprise/voicemail/messages", "get").includes("principalId"));
  assert.ok(params("/v1/enterprise/forwarding-rules", "get").includes("principalId"));
  assert.ok(params("/v1/calls/{call_id}/recording-authorizations", "get").includes("principalId"));
  assert.deepEqual(Object.keys(enterprise.RecordingConsentRequest.properties).sort(), ["decision", "endpointId"]);
  assert.deepEqual(Object.keys(enterprise.RecordingActorRequest.properties), ["principalId"]);
  assert.deepEqual(enterprise.CreateForwardingRuleRequest.required.sort(), ["condition", "destination", "principalId"]);
  assert.deepEqual(Object.keys(enterprise.VoicemailMediaAccessRequest.properties), ["principalId"]);
});

test("Hub call launch and contacts are the served Communications operations, with the contract's body", () => {
  const paths = requestedPaths(source("src/services/calling/hub-calls.service.ts")).filter((p) => p.path.startsWith("/communications/"));
  assert.ok(paths.length >= 3);
  for (const { method, path } of paths) assert.ok(has(hubOps, method, `/api/v2${path}`), `${method ?? "ANY"} /api/v2${path}`);
  assert.ok(has(hubOps, "POST", "/api/v2/communications/contacts/{id}/calls"));
  assert.ok(has(hubOps, "POST", "/api/v2/communications/conversations/{id}/calls"));
  const launch = hub.components.schemas.CallLaunch;
  assert.equal(launch.additionalProperties, false);
  assert.deepEqual(launch.required.sort(), ["clientRequestId", "endpointId", "kind"]);
  const text = source("src/services/calling/hub-calls.service.ts");
  assert.match(text, /const body: HubCallLaunch = \{ kind, clientRequestId, endpointId \}/);
  assert.deepEqual(launch.properties.kind.enum, ["VOICE", "VIDEO"]);
});

test("organization calling uses Meeting's relay and only paths the relay allowlists", () => {
  for (const method of METHODS) assert.ok(has(meetingOps, method.toUpperCase(), "/api/v2/organizations/{id}/calling/{path}"), method);
  assert.ok(has(meetingOps, "POST", "/api/v2/messaging/principals/names"));
  assert.match(source("src/services/calling/organization-calling.service.ts"), /url: `\/organizations\/\$\{encodeURIComponent\(organizationId\)\}\/calling\/\$\{path\}`/);
  // Every path the admin view reads is a Calling operation Meeting relays (services/meeting-service/src/services/organization_calling.rs).
  const relay = readFileSync(new URL("services/meeting-service/src/services/organization_calling.rs", root), "utf8");
  const admin = source("src/components/calling/OrganizationCallingAdmin.tsx");
  const used = [...admin.matchAll(/"(v1\/[a-z/-]+)"/g)].map((m) => m[1]);
  assert.ok(used.length >= 5);
  for (const path of used) {
    assert.ok(has(callingOps, null, `/${path}`), `/${path} in Calling contract`);
    const literal = path.replace(/^v1\/enterprise\//, "");
    assert.ok(relay.includes(`"v1/${path.slice(3)}"`) || relay.includes(literal.split("/")[0]), `${path} relayed by Meeting`);
  }
  assert.ok(relay.includes('"v1/enterprise/recordings/{}/media-access"'));
});

/** Quoted members of `export type Name = "A" | "B";` in a TS file. */
function union(file, name) {
  const m = new RegExp(`export type ${name} =([^;]+);`).exec(source(file));
  assert.ok(m, `${name} in ${file}`);
  return [...m[1].matchAll(/"([A-Z_]+)"/g)].map((x) => x[1]).sort();
}
const sorted = (values) => values.filter((v) => v !== null).slice().sort();

test("mirror types carry exactly the contract enums", () => {
  const types = "src/types/calling/";
  assert.deepEqual(union(`${types}recording.ts`, "ConsentDecision"), sorted(enterprise.ConsentDecision.enum));
  assert.deepEqual(union(`${types}recording.ts`, "RecordingState"), sorted(enterprise.RecordingState.enum));
  assert.deepEqual(union(`${types}recording.ts`, "RecordingStopReason"), sorted(enterprise.RecordingStopReason.enum));
  assert.deepEqual(union(`${types}recording.ts`, "RecordingAuthorizationReason"), sorted(enterprise.RecordingAuthorizationReason.enum));
  assert.deepEqual(union(`${types}forwarding.ts`, "ForwardingCondition"), sorted(enterprise.ForwardingCondition.enum));
  assert.deepEqual(union(`${types}forwarding.ts`, "DestinationType"), sorted(enterprise.DestinationType.enum));
  assert.deepEqual(union(`${types}voicemail.ts`, "MailboxOwnerType"), sorted(enterprise.MailboxOwnerType.enum));
  assert.deepEqual(union(`${types}voicemail.ts`, "VoicemailReadState"), sorted(enterprise.VoicemailReadState.enum));
  assert.deepEqual(union(`${types}voicemail.ts`, "VoicemailMessageState"), sorted(enterprise.VoicemailMessageState.enum));
  assert.deepEqual(union(`${types}common.ts`, "CallState"), sorted(common.CallState.enum));
  assert.deepEqual(union(`${types}common.ts`, "CallKind"), sorted(common.CallKind.enum));
  assert.ok(control.RecoveredInvitation, "recovered offers come from the control contract");
});

test("abuse categories match the Calling handler (the contract types the body as a free object)", () => {
  const rust = readFileSync(new URL("services/calling/src/calls/app/abuse_reports.rs", root), "utf8");
  const message = /category must be ([A-Z_, ]+) or ([A-Z_]+)\./.exec(rust);
  assert.ok(message);
  const backend = [...message[1].split(",").map((s) => s.trim()), message[2]].sort();
  assert.deepEqual(union("src/types/calling/abuse.ts", "AbuseCategory"), backend);
  const adapter = source("src/app/api/calling/[...path]/route.ts");
  for (const category of backend) assert.ok(adapter.includes(`"${category}"`), `adapter accepts ${category}`);
});
