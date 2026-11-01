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
  vm.runInNewContext(source, { exports, require: name => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency ${name}`);
  } });
  return exports;
}

function services() {
  const requests = [];
  const api = axios.create({ headers: { Authorization: "Bearer user-token" }, adapter: async config => {
    requests.push(config);
    return { config, status: 200, statusText: "OK", headers: {}, data: {} };
  } });
  const dependencies = { "./api": { __esModule: true, default: api } };
  return {
    requests,
    profiles: load("../src/services/v2/profiles.service.ts", dependencies).profilesService,
    quality: load("../src/services/v2/quality.service.ts", dependencies).qualityService,
  };
}

const plain = value => JSON.parse(JSON.stringify(value));

test("registration and backstage services use the exact authenticated v2 routes", async () => {
  const { profiles, requests } = services();
  const registration = {
    displayName: "Attendee",
    conferenceId: "conf/1",
    email: "attendee@example.test",
    organization: "Example",
    customFields: { department: "Research" },
    idempotencyKey: "key-1",
  };
  await profiles.register("space/1", registration);
  await profiles.listRegistrations("space/1");
  await profiles.getRegistration("space/1", "registration/1");
  await profiles.cancelRegistration("space/1", "registration/1");
  await profiles.enterBackstage("conf/1");
  await profiles.leaveBackstage("conf/1");
  assert.deepEqual(requests.map(request => [request.method, request.url]), [
    ["post", "/meeting-spaces/space%2F1/registrations"],
    ["get", "/meeting-spaces/space%2F1/registrations"],
    ["get", "/meeting-spaces/space%2F1/registrations/registration%2F1"],
    ["delete", "/meeting-spaces/space%2F1/registrations/registration%2F1"],
    ["post", "/conferences/conf%2F1/backstage"],
    ["delete", "/conferences/conf%2F1/backstage"],
  ]);
  assert.deepEqual(plain(JSON.parse(requests[0].data)), registration);
  assert.equal(requests[0].headers.Authorization, "Bearer user-token");
  assert.equal(requests[4].data, undefined);
});

test("quality service submits backend-shaped samples and requests the authorized summary route", async () => {
  const { quality, requests } = services();
  const batch = {
    participantSessionId: "session-1",
    batchId: "batch-1",
    samples: [{ timestamp: "2030-01-01T00:00:00.000Z", rttMs: 42, jitterMs: 5, packetLossRatio: 0.01 }],
  };
  await quality.submitSamples("conference/1", batch);
  await quality.getQuality("conference/1");
  assert.deepEqual(requests.map(request => [request.method, request.url]), [
    ["post", "/conferences/conference%2F1/quality/samples"],
    ["get", "/conferences/conference%2F1/quality"],
  ]);
  assert.deepEqual(plain(JSON.parse(requests[0].data)), batch);
});

test("backstage and quality UI gates match backend role defaults", () => {
  const authority = load("../src/lib/meeting-authority.ts");
  for (const role of ["OWNER", "HOST", "CO_HOST", "MODERATOR", "PRESENTER"]) {
    assert.equal(authority.meetingAllows(role, "backstage.enter"), true, role);
  }
  for (const role of ["PARTICIPANT", "VIEWER", undefined]) {
    assert.equal(authority.meetingAllows(role, "backstage.enter"), false, String(role));
  }
  for (const role of ["OWNER", "HOST", "CO_HOST"]) {
    assert.equal(authority.meetingAllows(role, "quality.view"), true, role);
  }
  assert.equal(authority.meetingAllows("MODERATOR", "quality.view"), true);
  assert.equal(authority.meetingAllows("MODERATOR", "quality.view", { overrides: { MODERATOR: { "quality.view": false } } }), false);
  assert.equal(authority.meetingAllows("PRESENTER", "quality.view"), false);
});

test("registration token stays in local component state and uses the invitation fragment flow", () => {
  const form = readFileSync(new URL("../src/components/meeting-spaces/PublicRegistrationForm.tsx", import.meta.url), "utf8");
  assert.match(form, /profilesService\.register/);
  assert.doesNotMatch(form, /useMutation/);
  assert.match(form, /\/invite#\$\{encodeURIComponent\(result\.admissionToken\)\}/);
  assert.match(form, /idempotencyKey:\s*idempotency\.current\.key/);
});

test("V2 media context and specialized DTO types follow backend serialization", () => {
  const types = readFileSync(new URL("../src/types/v2/index.ts", import.meta.url), "utf8");
  const source = readFileSync(new URL("../../../services/meeting-service/src/services/collaboration/breakouts.rs", import.meta.url), "utf8");
  const registration = readFileSync(new URL("../../../services/meeting-service/src/services/specialized.rs", import.meta.url), "utf8");
  assert.match(source, /pub struct MediaContextView\s*\{\s*[\s\S]*?pub kind:\s*String,\s*pub breakout_id:\s*Option<Uuid>,\s*pub visit:\s*bool/);
  assert.match(types, /export interface MediaContextView\s*\{\s*kind:\s*"MAIN"\s*\|\s*"BREAKOUT"\s*\|\s*"BACKSTAGE";\s*breakoutId:\s*string\s*\|\s*null;\s*visit:\s*boolean;/);
  assert.match(registration, /pub struct BackstageResponse\s*\{\s*pub participant_id:\s*Uuid,\s*pub in_backstage:\s*bool,\s*pub media_context:\s*MediaContextView,\s*[\s\S]*?pub credential_refresh_required:\s*bool/);
});
