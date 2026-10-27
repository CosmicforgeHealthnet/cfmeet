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
  vm.runInNewContext(source, { exports, URL, Date, require: name => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency ${name}`);
  } });
  return exports;
}
const authority = load("../src/lib/meeting-authority.ts");
const plain = value => JSON.parse(JSON.stringify(value));

test("invitation availability reflects revocation, expiry and transactional use bounds", () => {
  const now = Date.parse("2030-01-01T00:00:00Z");
  const invitation = { status: "ACTIVE", expiresAt: "2030-01-02T00:00:00Z", useLimit: 2, usageCount: 1 };
  assert.equal(authority.invitationAvailability(invitation, now), "Active");
  assert.equal(authority.invitationAvailability({ ...invitation, usageCount: 2 }, now), "Use limit reached");
  assert.equal(authority.invitationAvailability({ ...invitation, useLimit: null, usageCount: 100 }, now), "Active");
  assert.equal(authority.invitationAvailability(invitation, Date.parse(invitation.expiresAt)), "Expired");
  assert.equal(authority.invitationAvailability({ ...invitation, status: "REVOKED" }, now), "Revoked");
});

test("invitations respect grant authority, including delegated co-host assignment", () => {
  assert.equal(authority.invitationRoles("OWNER").includes("OWNER"), false);
  assert.equal(authority.invitationRoles("OWNER").includes("HOST"), true);
  assert.equal(authority.invitationRoles("HOST").includes("HOST"), false);
  assert.equal(authority.invitationRoles("CO_HOST").includes("CO_HOST"), false);
  assert.equal(authority.invitationRoles("MODERATOR").includes("PARTICIPANT"), true);
  assert.equal(authority.invitationRoles("PARTICIPANT").length, 0);
  assert.equal(authority.invitationRoles("VIEWER").length, 0);
  const delegated = { overrides: { CO_HOST: { "cohost.assign": true } } };
  assert.equal(authority.invitationRoles("CO_HOST", delegated).includes("CO_HOST"), true);
  assert.equal(authority.invitationRoles("MODERATOR", { overrides: { MODERATOR: { "participant.admit": false } } }).length, 0);
});

test("participant moderation excludes self, owner and equal/higher roles; policy cells are respected", () => {
  assert.equal(authority.effectiveConferenceRole("OWNER", "PARTICIPANT"), "OWNER");
  assert.equal(authority.effectiveConferenceRole("PARTICIPANT", "MODERATOR"), "MODERATOR");
  assert.equal(authority.effectiveConferenceRole(undefined, "VIEWER"), "VIEWER");
  assert.equal(authority.effectiveConferenceRole("OWNER", "PARTICIPANT", "BANNED"), undefined);
  for (const target of ["OWNER", "HOST"]) assert.equal(authority.mayTarget("HOST", target, false), false);
  assert.equal(authority.mayTarget("HOST", "PARTICIPANT", true), false);
  assert.equal(authority.mayTarget("MODERATOR", "PARTICIPANT", false), true);
  assert.equal(authority.meetingAllows("MODERATOR", "participant.remove"), true);
  assert.equal(authority.meetingAllows("MODERATOR", "participant.role_change"), false);
  assert.equal(authority.meetingAllows("CO_HOST", "participant.ban"), false);
  assert.equal(authority.meetingAllows("CO_HOST", "participant.ban", { overrides: { CO_HOST: { "participant.ban": true } } }), true);
  assert.equal(authority.participantRoles("MODERATOR").length, 0);
  assert.equal(authority.participantRoles("MODERATOR", { overrides: { MODERATOR: { "participant.role_change": true } } }).includes("PRESENTER"), true);
  assert.equal(authority.meetingAllows("VIEWER", "participant.remove", { overrides: { VIEWER: { "participant.remove": true } } }), false);
});

test("invitation drafts validate limits and emit only backend fields", () => {
  const draft = { role: "PARTICIPANT", conferenceId: "c", email: " Person@Example.com ", hours: "24", limit: "2", ownerUserId: "forged", token: "forged" };
  assert.deepEqual(plain(authority.invitationInput(draft)), { role: "PARTICIPANT", conferenceId: "c", inviteeEmail: "person@example.com", expiresInHours: 24, useLimit: 2 });
  const unlimited = authority.invitationInput({ ...draft, email: "", conferenceId: "", limit: "" });
  for (const field of ["useLimit", "inviteeEmail", "conferenceId"]) assert.equal(field in unlimited, false);
  for (const value of [{ role: "OWNER" }, { hours: "0" }, { hours: "1.5" }, { limit: "10001" }, { limit: "0" }, { email: "invalid" }]) assert.throws(() => authority.invitationInput({ ...draft, ...value }));
});

test("invitation links extract fragment tokens and reject paths, query tokens and traversal", () => {
  assert.equal(authority.invitationToken("https://meet.example/invite#inv_ABC-123"), "inv_ABC-123");
  assert.equal(authority.invitationToken(" inv_ABC-123 "), "inv_ABC-123");
  for (const input of ["https://meet.example/invite?token=secret", "../../admin", "", "x".repeat(129), "a/b", "javascript:alert(1)"]) assert.throws(() => authority.invitationToken(input));
});

function services() {
  const requests = [];
  const make = authenticated => axios.create({ ...(authenticated ? { headers: { Authorization: "Bearer account" } } : {}), adapter: async config => {
    requests.push(config); return { config, status: 200, statusText: "OK", headers: {}, data: { providerEnforcement: "PENDING_PROVIDER_CONTROL" } };
  } });
  const dependencies = { "./api": { __esModule: true, default: make(true), publicApiV2: make(false) } };
  return { requests, invitations: load("../src/services/v2/invitations.service.ts", dependencies).invitationsService, participants: load("../src/services/v2/participants.service.ts", dependencies).participantsService };
}

test("all invitation operations use scoped v2 routes; resolving is public and never joins", async () => {
  const { invitations: inv, requests } = services();
  await inv.list("space/1"); await inv.create("s", { role: "VIEWER", conferenceId: "c", expiresInHours: 24, useLimit: 1 }); await inv.revoke("s", "i/1"); await inv.resolve("inv/test");
  assert.deepEqual(requests.map(r => [r.method, r.url]), [["get", "/meeting-spaces/space%2F1/invitations"], ["post", "/meeting-spaces/s/invitations"], ["post", "/meeting-spaces/s/invitations/i%2F1/revoke"], ["post", "/invitations/inv%2Ftest/resolve"]]);
  assert.equal(requests[0].headers.Authorization, "Bearer account");
  assert.equal(requests.at(-1).headers.Authorization, undefined);
  assert.equal(requests.at(-1).headers["X-Participant-Token"], undefined);
  assert.deepEqual(JSON.parse(requests.at(-1).data), {});
  assert.equal(requests.some(r => r.url.endsWith("/join")), false);
});

test("moderation sends scoped participant ids, reviewed versions and provider commands", async () => {
  const { participants: p, requests } = services();
  await p.list("c/1"); await p.me("c/1");
  await p.changeRole("c/1", "p/1", { role: "VIEWER", version: 0 });
  const remove = await p.remove("c/1", "p/1", { reason: "Disruption" });
  await p.ban("c/1", "p/1", { scope: "SPACE", reason: "Abuse", expiresAt: "2090-01-01T00:00:00Z" });
  await p.mute("c/1", "p/1", { trackKind: "AUDIO" });
  assert.deepEqual(requests.map(r => [r.method, r.url]), [
    ["get", "/conferences/c%2F1/participants"], ["get", "/conferences/c%2F1/participants/me"],
    ["patch", "/conferences/c%2F1/participants/p%2F1/role"], ["post", "/conferences/c%2F1/participants/p%2F1/remove"],
    ["post", "/conferences/c%2F1/participants/p%2F1/ban"], ["post", "/conferences/c%2F1/participants/p%2F1/mute"],
  ]);
  assert.deepEqual(JSON.parse(requests[2].data), { role: "VIEWER", version: 0 });
  assert.equal(JSON.parse(requests[4].data).scope, "SPACE");
  assert.deepEqual(JSON.parse(requests[5].data), { trackKind: "AUDIO" });
  assert.equal(remove.providerEnforcement, "PENDING_PROVIDER_CONTROL");
});

test("moderation conflicts refresh both roster and own authority; secret operations never use query keys", async () => {
  const invalidated = [];
  const client = { invalidateQueries: async ({ queryKey }) => invalidated.push(queryKey) };
  const reactQuery = { useMutation: options => options, useQueryClient: () => client };
  const hooks = load("../src/hooks/v2/useParticipants.ts", { "@tanstack/react-query": reactQuery, "@/services/v2/participants.service": { participantsService: {} } });
  for (const name of ["useChangeRole", "useRemoveParticipant", "useBanParticipant", "useAdmitParticipants", "useDenyParticipants"]) {
    invalidated.length = 0; const mutation = hooks[name]("c");
    assert.equal(mutation.onMutate, undefined);
    await mutation.onSettled(undefined, new Error("version conflict"));
    assert.deepEqual(plain(invalidated), [["v2", "participants"]]);
  }
  const invitations = load("../src/hooks/v2/useInvitations.ts", { "@tanstack/react-query": reactQuery, "@/services/v2/invitations.service": { invitationsService: {} } });
  assert.equal(invitations.useResolveInvitation().gcTime, 0);
  assert.equal(invitations.useResolveInvitation().queryKey, undefined);
  assert.equal(invitations.useCreateInvitation("s").gcTime, 0);
  assert.equal("resolve" in invitations.invitationKeys, false);
});

test("invitation and participant enums match the backend vocabulary", () => {
  const rust = readFileSync(new URL("../../../services/meeting-service/src/domain/mod.rs", import.meta.url), "utf8");
  const frontend = readFileSync(new URL("../src/types/v2/index.ts", import.meta.url), "utf8");
  for (const name of ["InvitationStatus", "AdmissionState", "BanScope"]) {
    const values = [...rust.match(new RegExp(`\\b${name}\\s*\\{([^}]+)\\}`))[1].matchAll(/=\s*"([A-Z_]+)"/g)].map(m => m[1]).sort();
    const actual = [...frontend.match(new RegExp(`export type ${name}\\s*=\\s*([\\s\\S]*?);`))[1].matchAll(/"([A-Z_]+)"/g)].map(m => m[1]).sort();
    assert.deepEqual(actual, values, name);
  }
  const provider = readFileSync(new URL("../../../services/meeting-service/src/services/rtc_provider.rs", import.meta.url), "utf8");
  const trackKinds = [...provider.matchAll(/TrackKind::\w+ => "([A-Z_]+)"/g)].map(m => m[1]).sort();
  const clientKinds = [...frontend.match(/export type TrackKind\s*=\s*([^;]+);/)[1].matchAll(/"([A-Z_]+)"/g)].map(m => m[1]).sort();
  assert.deepEqual(clientKinds, trackKinds);
});
