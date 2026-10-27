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
  vm.runInNewContext(source, { exports, Date, Intl, process: { env: {} }, require: name => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency: ${name}`);
  } });
  return exports;
}
const scheduling = load("../src/lib/meeting-scheduling.ts");
const plain = value => JSON.parse(JSON.stringify(value));

test("recurrence preserves wall time across DST boundaries and never serializes an offset", () => {
  const body = scheduling.recurrenceInput("RRULE:FREQ=WEEKLY;BYDAY=SU;COUNT=12", "2027-03-07T09:30", 60);
  assert.deepEqual(plain(body), { rrule: "FREQ=WEEKLY;BYDAY=SU;COUNT=12", startLocal: "2027-03-07T09:30:00", durationMinutes: 60 });
  assert.throws(() => scheduling.recurrenceInput("FREQ=DAILY", "2027-03-07T09:30:00Z", 60));
  for (const duration of [0, 1441, 1.5, NaN]) assert.throws(() => scheduling.recurrenceInput("FREQ=DAILY", "2027-03-07T09:30", duration));
});

test("individual occurrences use absolute timestamps and reject invalid or past windows", () => {
  assert.equal(scheduling.occurrenceTimestamp("2090-01-04T09:00:00+01:00"), "2090-01-04T08:00:00.000Z");
  assert.throws(() => scheduling.occurrenceTimestamp("2090-01-04T09:00:00"));
  assert.throws(() => scheduling.occurrenceTimestamp("2000-01-04T09:00:00Z"));
  const body = scheduling.scheduledWindow("2090-01-04T09:00:00+01:00", "2090-01-04T10:00:00+01:00");
  assert.equal(body.scheduledStartAt, "2090-01-04T08:00:00.000Z");
  for (const end of ["2090-01-04T08:00:00+01:00", "2090-01-06T09:00:00+01:00", "invalid"]) assert.throws(() => scheduling.scheduledWindow("2090-01-04T09:00:00+01:00", end));
});

test("management permissions respect owner transfer boundaries and co-host policy overrides", () => {
  assert.equal(scheduling.grantableRoles("OWNER").includes("HOST"), true);
  assert.equal(scheduling.grantableRoles("HOST").includes("HOST"), false);
  for (const role of ["CO_HOST", "MODERATOR", "PARTICIPANT", undefined]) assert.equal(scheduling.grantableRoles(role).length, 0);
  assert.equal(scheduling.grantableRoles("OWNER").includes("OWNER"), false);
  const space = { status: "ACTIVE", view: "FULL", viewerRole: "CO_HOST" };
  assert.equal(scheduling.conferencePermission(space, "start"), true);
  assert.equal(scheduling.conferencePermission(space, "lock"), true);
  assert.equal(scheduling.conferencePermission(space, "end"), false);
  const configured = { ...space, rolePolicy: { overrides: { CO_HOST: { "conference.start": false, "conference.end": true } } } };
  assert.equal(scheduling.conferencePermission(configured, "start"), false);
  assert.equal(scheduling.conferencePermission(configured, "end"), true);
  assert.equal(scheduling.conferencePermission({ ...space, view: "SUMMARY" }, "start"), false);
  for (const status of ["ENDING", "ENDED", "CANCELLED", "FAILED"]) {
    assert.equal(scheduling.joinable(status), false); assert.equal(scheduling.unstarted(status), false);
  }
});

function services() {
  const requests = [];
  const api = axios.create({ adapter: async config => {
    requests.push(config);
    return { status: config.method === "delete" && config.url.endsWith("/recurrence") ? 204 : 200, statusText: "OK", headers: {}, config, data: config.method === "delete" && config.url.endsWith("/recurrence") ? undefined : { conference: { id: "c", status: "ENDING" } } };
  } });
  const dependencies = { "./api": { __esModule: true, default: api } };
  return {
    requests,
    conferences: load("../src/services/v2/conferences.service.ts", dependencies).conferencesService,
    members: load("../src/services/v2/members.service.ts", dependencies).membersService,
    spaces: load("../src/services/v2/spaces.service.ts", dependencies).spacesService,
  };
}

test("conference lifecycle, scheduling and own-session leave use the backend methods and payloads", async () => {
  const { conferences: c, requests } = services();
  await c.list("s", { from: "2090-01-01T00:00:00Z", to: "2090-02-01T00:00:00Z" });
  await c.get("c/1"); await c.create("s", { scheduledStartAt: "2090-01-04T08:00:00Z" });
  for (const action of ["prepare", "start", "lock", "unlock", "end", "cancel"]) {
    const result = await c[action]("c"); assert.equal(result.conference.status, "ENDING");
  }
  await c.reschedule("c", { scheduledStartAt: "2090-01-05T08:00:00Z" });
  await c.join("c", { passcode: "secret", previousSessionId: "session", consents: [{ consentType: "RECORDING", granted: true }] });
  await c.leave("c", { participantSessionId: "session" });
  assert.deepEqual(requests.map(r => [r.method, r.url]), [
    ["get", "/meeting-spaces/s/conferences"], ["get", "/conferences/c%2F1"], ["post", "/meeting-spaces/s/conferences"],
    ...["prepare", "start", "lock", "unlock", "end", "cancel", "reschedule", "join", "leave"].map(action => ["post", `/conferences/c/${action}`]),
  ]);
  for (const r of requests.slice(3, 9)) assert.deepEqual(JSON.parse(r.data), {});
  assert.deepEqual(JSON.parse(requests.at(-1).data), { participantSessionId: "session" });
  assert.equal(requests[0].params.from, "2090-01-01T00:00:00Z");
});

test("recurrence deletion accepts 204, exceptions and versioned memberships use their dedicated routes", async () => {
  const { spaces: s, members: m, requests } = services();
  await s.getRecurrence("s"); await s.setRecurrence("s", scheduling.recurrenceInput("FREQ=DAILY;COUNT=5", "2090-01-04T09:00", 60));
  assert.equal(await s.deleteRecurrence("s"), undefined);
  await s.getExceptions("s"); await s.setException("s", { originalStartAt: "2090-01-04T08:00:00Z", action: "CANCEL" });
  await m.list("s"); await m.add("s", { email: "colleague@example.com", role: "PARTICIPANT" });
  await m.update("s", "m/1", { role: "VIEWER", version: 0 }); await m.revoke("s", "m/1");
  assert.deepEqual(requests.map(r => [r.method, r.url]), [
    ["get", "/meeting-spaces/s/recurrence"], ["put", "/meeting-spaces/s/recurrence"], ["delete", "/meeting-spaces/s/recurrence"],
    ["get", "/meeting-spaces/s/recurrence/exceptions"], ["put", "/meeting-spaces/s/recurrence/exceptions"],
    ["get", "/meeting-spaces/s/members"], ["post", "/meeting-spaces/s/members"], ["patch", "/meeting-spaces/s/members/m%2F1"], ["delete", "/meeting-spaces/s/members/m%2F1"],
  ]);
  assert.deepEqual(JSON.parse(requests[7].data), { role: "VIEWER", version: 0 });
  assert.equal("scheduledStartAt" in JSON.parse(requests[4].data), false);
});

test("missing recurrence is an empty state; permission, network and other 404 errors stay visible", async () => {
  let failure;
  const hooks = load("../src/hooks/v2/useSpaces.ts", {
    axios, "@tanstack/react-query": { useQuery: options => options },
    "@/services/v2/spaces.service": { spacesService: { getRecurrence: async () => { throw failure; } } },
  });
  const query = hooks.useSpaceRecurrence("s");
  failure = new axios.AxiosError("missing", "", undefined, undefined, { status: 404, data: { error: { code: "MEETING_NOT_FOUND" } } });
  assert.equal(await query.queryFn(), null);
  for (const status of [403, 500]) {
    failure = new axios.AxiosError("failed", "", undefined, undefined, { status, data: { error: { code: "PERMISSION_DENIED" } } });
    await assert.rejects(query.queryFn());
  }
  failure = new Error("offline"); await assert.rejects(query.queryFn());
});

test("schedule mutations refresh occurrences and resolution, and membership conflicts refresh reviewed data", async () => {
  const invalidated = [];
  const queryClient = { invalidateQueries: async ({ queryKey }) => invalidated.push(queryKey), setQueryData: () => {} };
  const reactQuery = { useMutation: options => options, useQueryClient: () => queryClient };
  const spaces = load("../src/hooks/v2/useSpaces.ts", { axios, "@tanstack/react-query": reactQuery, "@/services/v2/spaces.service": { spacesService: {} } });
  for (const name of ["useSetRecurrence", "useDeleteRecurrence", "useSetException"]) {
    invalidated.length = 0;
    await spaces[name]("s").onSettled(undefined, new Error("conflict"));
    assert.deepEqual(plain(invalidated), [["v2", "spaces"], ["v2", "conferences"]]);
  }
  const members = load("../src/hooks/v2/useMembers.ts", { "@tanstack/react-query": reactQuery, "@/services/v2/members.service": { membersService: {} } });
  invalidated.length = 0;
  await members.useUpdateMember("s").onSettled(undefined, new Error("version conflict"));
  assert.ok(invalidated.some(key => key[1] === "members"));
  assert.ok(invalidated.some(key => key[1] === "spaces"));
  const conferences = load("../src/hooks/v2/useConferences.ts", { "@tanstack/react-query": reactQuery, "@/services/v2/conferences.service": { conferencesService: {} }, "./useSpaces": spaces });
  const end = conferences.useEndConference();
  assert.equal(end.onMutate, undefined);
  let saved;
  queryClient.setQueryData = (_key, value) => { saved = value; };
  end.onSuccess({ conference: { id: "c", status: "ENDING" } });
  assert.equal(saved.status, "ENDING");
});

test("conference, recurrence and membership enum vocabulary matches the Rust domain", () => {
  const rust = readFileSync(new URL("../../../services/meeting-service/src/domain/mod.rs", import.meta.url), "utf8");
  const frontend = readFileSync(new URL("../src/types/v2/index.ts", import.meta.url), "utf8");
  for (const type of ["ConferenceStatus", "ConferenceKind", "TerminationReason", "IdentityType", "RecurrenceStatus", "RecurrenceExceptionKind", "PrincipalType", "MemberStatus"]) {
    const backendBlock = rust.match(new RegExp(`\\b${type}\\s*\\{([^}]+)\\}`))[1];
    const backend = [...backendBlock.matchAll(/=\s*"([A-Z_]+)"/g)].map(m => m[1]).sort();
    const clientBlock = frontend.match(new RegExp(`export type ${type}\\s*=\\s*([\\s\\S]*?);`))[1];
    assert.deepEqual([...clientBlock.matchAll(/"([A-Z_]+)"/g)].map(m => m[1]).sort(), backend, type);
  }
});

test("a device's previous conference credential cannot accompany another occurrence join", async () => {
  const requests = [];
  const api = load("../src/services/v2/api.ts", {
    axios: { create: config => axios.create({ ...config, adapter: async request => {
      requests.push(request); return { status: 200, statusText: "OK", headers: {}, config: request, data: {} };
    } }) },
    "@/store": {
      cookieStore: { getAccessToken: () => null },
      storageStore: { getParticipantToken: () => "conference-a-token", getV2JoinData: () => ({ conference: { id: "a" } }) },
    },
  }).default;
  await api.post("/conferences/a/leave", { participantSessionId: "session-a" });
  await api.post("/conferences/b/join", { displayName: "Guest" });
  await api.get("/meeting-spaces/s/members");
  assert.equal(requests[0].headers["X-Participant-Token"], "conference-a-token");
  assert.equal(requests[1].headers["X-Participant-Token"], undefined);
  assert.equal(requests[2].headers["X-Participant-Token"], undefined);
});
