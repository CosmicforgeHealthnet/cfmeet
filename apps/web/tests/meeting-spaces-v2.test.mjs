import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import axios from "axios";

function load(path, dependencies = {}) {
  const exports = {};
  const source = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  vm.runInNewContext(source, { exports, require: (name) => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency: ${name}`);
  }, process: { env: {} }, URL, Intl });
  return exports;
}

const helpers = load("../src/lib/meeting-spaces.ts");
const id = "11111111-1111-4111-8111-111111111111";
const space = { id, title: "Planning", description: "", meetingCode: "PLAN1234", status: "ACTIVE", view: "FULL", viewerRole: "OWNER", version: 7, accessPolicy: "PUBLIC", meetingType: "STANDARD", timezone: "Africa/Lagos", waitingRoomPolicy: "OFF", requiresPasscode: false };
const form = { title: "Updated planning", description: "New description", meetingType: "STANDARD", timezone: "Africa/Lagos", waitingRoomPolicy: "OFF", accessPolicy: "PUBLIC", passcode: "", clearPasscode: false };

function client(login = "login-token") {
  const requests = [];
  const api = load("../src/services/v2/api.ts", {
    axios: { create: (config) => axios.create({ ...config, adapter: async (request) => {
      requests.push(request);
      return { status: 200, statusText: "OK", headers: {}, config: request, data: space };
    } }) },
    "@/store": { cookieStore: { getAccessToken: () => login }, storageStore: { getParticipantToken: () => "old-conference-participant-token" } },
  });
  return { requests, api: api.default, service: load("../src/services/v2/spaces.service.ts", { "./api": api }).spacesService };
}

test("every listed meeting-space operation uses v2, camelCase and dedicated versioned commands", async () => {
  const c = client();
  await c.service.list(); await c.service.get("legacy/code");
  await c.service.create({ title: "New space", accessPolicy: "AUTHENTICATED" });
  await c.service.update(id, helpers.spaceUpdate(space, form));
  await c.service.archive(id, 0); await c.service.duplicate(id, { title: "Copy" });
  await c.service.transferOwnership(id, { newOwnerUserId: "target", version: 7, retainPreviousOwner: false });
  await c.service.createInstant({ title: "Live now" }); await c.service.resolve("legacy/code");
  assert.deepEqual(c.requests.map((r) => [r.method, r.url]), [
    ["get", "/meeting-spaces"], ["get", "/meeting-spaces/legacy%2Fcode"], ["post", "/meeting-spaces"],
    ["patch", `/meeting-spaces/${id}`], ["post", `/meeting-spaces/${id}/archive`],
    ["post", `/meeting-spaces/${id}/duplicate`], ["post", `/meeting-spaces/${id}/owner-transfer`],
    ["post", "/instant-meetings"], ["get", "/resolve/legacy%2Fcode"],
  ]);
  for (const request of c.requests) {
    assert.equal(request.baseURL, "http://localhost:8080/api/v2");
    assert.equal(request.headers.Authorization, "Bearer login-token");
    assert.equal(request.headers["X-Participant-Token"], undefined);
  }
  assert.equal(JSON.parse(c.requests[3].data).version, 7);
  assert.equal(JSON.parse(c.requests[4].data).version, 0);
  assert.deepEqual(JSON.parse(c.requests[6].data), { newOwnerUserId: "target", version: 7, retainPreviousOwner: false });
  assert.equal("ownerUserId" in JSON.parse(c.requests[2].data), false);
});

test("resolution works anonymously and a previous conference token never grants space privileges", async () => {
  const guest = client(null);
  await guest.service.resolve("PLAN1234");
  assert.equal(guest.requests[0].headers.Authorization, undefined);
  assert.equal(guest.requests[0].headers["X-Participant-Token"], undefined);
  await guest.api.get("/me/organizations");
  await guest.api.get("/organizations/org-id/members");
  assert.equal(guest.requests[1].headers["X-Participant-Token"], undefined);
  assert.equal(guest.requests[2].headers["X-Participant-Token"], undefined);
  await guest.api.get(`/conferences/${id}/participants`);
  assert.equal(guest.requests[3].headers["X-Participant-Token"], "old-conference-participant-token");
});

test("a summary/link grants no configuration rights; only owners get archive/transfer rights", () => {
  assert.equal(helpers.spacePermissions(space).owner, true);
  assert.equal(helpers.spacePermissions({ ...space, viewerRole: "HOST" }).configure, true);
  assert.equal(helpers.spacePermissions({ ...space, viewerRole: "HOST" }).owner, false);
  for (const role of [undefined, "CO_HOST", "MODERATOR", "PRESENTER", "PARTICIPANT", "VIEWER"]) {
    assert.equal(helpers.spacePermissions({ ...space, viewerRole: role }).configure, false);
  }
  assert.equal(helpers.spacePermissions({ ...space, view: "SUMMARY" }).owner, false);
  assert.equal(helpers.spacePermissions({ ...space, view: "SUMMARY" }).configure, false);
  assert.throws(() => helpers.spaceUpdate({ ...space, view: "SUMMARY" }, form));
  assert.throws(() => helpers.spaceUpdate({ ...space, status: "ARCHIVED" }, form));
  assert.throws(() => helpers.spaceUpdate({ ...space, version: undefined }, form));
});

test("editor preserves the reviewed version and never patches conference dates, roles or ownership", () => {
  const body = helpers.spaceUpdate(space, { ...form, ownerUserId: "forged", viewerRole: "OWNER", scheduledStartAt: "tomorrow", start_time: "tomorrow" });
  assert.equal(body.version, 7);
  for (const key of ["ownerUserId", "viewerRole", "scheduledStartAt", "start_time", "end_time", "metadata", "is_private"]) assert.equal(key in body, false);
  assert.equal("passcode" in body, false);
  assert.equal(helpers.spaceUpdate({ ...space, version: 0 }, form).version, 0);
});

test("passcodes are preserved unless explicitly replaced/cleared and invalid drafts are refused", () => {
  const protectedSpace = { ...space, accessPolicy: "PUBLIC_PASSCODE", requiresPasscode: true };
  const protectedForm = { ...form, accessPolicy: "PUBLIC_PASSCODE" };
  assert.equal("passcode" in helpers.spaceUpdate(protectedSpace, protectedForm), false);
  assert.equal(helpers.spaceUpdate(protectedSpace, { ...form, clearPasscode: true }).clearPasscode, true);
  for (const invalid of [{ title: " " }, { title: "x".repeat(256) }, { timezone: "Invalid/Timezone" }, { passcode: "123" }, { passcode: "1234", clearPasscode: true }]) assert.throws(() => helpers.spaceUpdate(space, { ...form, ...invalid }));
  assert.throws(() => helpers.spaceUpdate(space, protectedForm));
  assert.throws(() => helpers.spaceUpdate(protectedSpace, { ...protectedForm, clearPasscode: true }));
});

test("meeting identifier normalization supports legacy IDs, v2 IDs/codes and links without granting roles", () => {
  for (const input of [id, "PLAN1234", `https://meet.example/${id}`, "https://meet.example/PLAN1234?ignored=1"]) assert.ok(helpers.meetingIdentifier(input));
  for (const input of ["", "../admin", "a/b", "javascript:alert(1)", "code?role=OWNER"]) assert.throws(() => helpers.meetingIdentifier(input));
});

test("frontend space policy vocabulary matches the backend domain enums", () => {
  const rust = readFileSync(new URL("../../../services/meeting-service/src/domain/mod.rs", import.meta.url), "utf8");
  const frontend = readFileSync(new URL("../src/types/v2/index.ts", import.meta.url), "utf8");
  for (const type of ["MeetingType", "SpaceStatus", "AccessPolicy", "WaitingRoomPolicy", "SecurityProfile", "Role", "MediaCapturePolicy"]) {
    const block = rust.match(new RegExp(`(?:text_enum!\\(\\s*(?:[\\s\\S]*?))?\\b${type}\\s*\\{([^}]+)\\}`))[1];
    const backend = [...block.matchAll(/=\s*"([A-Z_]+)"/g)].map((m) => m[1]).sort();
    const clientType = frontend.match(new RegExp(`export type ${type}\\s*=\\s*([\\s\\S]*?);`))[1];
    const clientValues = [...clientType.matchAll(/"([A-Z_]+)"/g)].map((m) => m[1]).sort();
    assert.deepEqual(clientValues, backend, type);
  }
});
