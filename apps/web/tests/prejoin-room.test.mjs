// Pre-join contract (FE-001) and the v2-only, conference-keyed room (FE-014 / MTG-019 / FE-017).
// Run: node --experimental-strip-types --test tests/prejoin-room.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import * as prejoin from "../src/lib/prejoin.ts";

const plain = (value) => JSON.parse(JSON.stringify(value));

// Shape of OpenAPI PreJoinResponse (services/meeting-service collaboration/prejoin.rs).
const preJoinResponse = (overrides = {}) => ({
  meetingSpaceId: "s1", conferenceId: "c1", title: "Clinic", meetingType: "HEALTHCARE", conferenceStatus: "LIVE", scheduledStartAt: null, joinable: true,
  organizer: { displayName: "Dr A" }, organization: null,
  access: { accessPolicy: "INVITE_ONLY", authenticationRequired: false, invitationRequired: true, passcodeRequired: false, externalAuthorization: false, waitingRoomExpected: true, alreadyAuthorized: false, ...(overrides.access ?? {}) },
  notices: { recordingPolicy: "HOST_OPTIONAL", transcriptionPolicy: "DISABLED", recordingActive: false, transcriptionActive: false, aiNotice: false },
  consents: { required: ["RECORDING", "HEALTHCARE_TERMS"], version: "2026-10", granted: [], ...(overrides.consents ?? {}) },
  media: {}, displayName: { suggested: null, editable: true, identityBearing: false }, devices: {},
});

test("pre-join requirements come from access.* and consents.* (FE-001)", () => {
  const req = prejoin.preJoinRequirements(preJoinResponse(), { signedIn: false });
  assert.deepEqual(plain(req), { passcode: false, invitation: true, signIn: false, waitingRoom: true, externalAuthorization: false, alreadyAuthorized: false, consents: ["RECORDING", "HEALTHCARE_TERMS"], consentVersion: "2026-10" });
  // Already granted consents (rejoin) are not asked again.
  assert.deepEqual(plain(prejoin.preJoinRequirements(preJoinResponse({ consents: { granted: ["RECORDING"] } }), { signedIn: true }).consents), ["HEALTHCARE_TERMS"]);
  // An invitation token from the link satisfies the invitation input.
  assert.equal(prejoin.preJoinRequirements(preJoinResponse(), { signedIn: false, hasInvitationToken: true }).invitation, false);
  // Sign-in is required only for signed-out callers.
  const auth = preJoinResponse({ access: { authenticationRequired: true } });
  assert.equal(prejoin.preJoinRequirements(auth, { signedIn: false }).signIn, true);
  assert.equal(prejoin.preJoinRequirements(auth, { signedIn: true }).signIn, false);
  // alreadyAuthorized waives passcode and invitation inputs.
  const authorized = prejoin.preJoinRequirements(preJoinResponse({ access: { passcodeRequired: true, alreadyAuthorized: true } }), { signedIn: true });
  assert.equal(authorized.passcode, false);
  assert.equal(authorized.invitation, false);
  // Without a pre-join answer only the space passcode flag is used.
  assert.equal(prejoin.preJoinRequirements(null, { signedIn: false, spaceRequiresPasscode: true }).passcode, true);
  assert.equal(prejoin.preJoinRequirements(undefined, { signedIn: false }).consents.length, 0);
});

test("join sends every checked required consent at the pre-join version and blocks until complete", () => {
  const req = prejoin.preJoinRequirements(preJoinResponse(), { signedIn: false });
  assert.deepEqual(plain(prejoin.consentGrants(req, { RECORDING: true })), [{ type: "RECORDING", version: "2026-10" }]);
  assert.deepEqual(plain(prejoin.consentGrants(req, { RECORDING: true, HEALTHCARE_TERMS: true, AI_PROCESSING: true })), [{ type: "RECORDING", version: "2026-10" }, { type: "HEALTHCARE_TERMS", version: "2026-10" }]);
  const input = { displayName: "Ada", passcode: "", invitationToken: "tok", checked: { RECORDING: true } };
  assert.match(prejoin.joinBlocker(req, input), /notices/);
  assert.equal(prejoin.joinBlocker(req, { ...input, checked: { RECORDING: true, HEALTHCARE_TERMS: true } }), null);
  assert.match(prejoin.joinBlocker(req, { ...input, invitationToken: " " }), /invitation/);
  assert.match(prejoin.joinBlocker(req, { ...input, displayName: "" }), /name/);
  const pass = prejoin.preJoinRequirements(preJoinResponse({ access: { passcodeRequired: true, invitationRequired: false }, consents: { required: [] } }), { signedIn: false });
  assert.match(prejoin.joinBlocker(pass, { displayName: "Ada", passcode: "", invitationToken: "", checked: {} }), /passcode/);
});

test("admission errors map to actionable messages", () => {
  const err = (code, message = "backend") => ({ response: { status: 403, data: { error: { code, message } } } });
  assert.match(prejoin.joinErrorMessage(err("CONSENT_REQUIRED")), /consent/);
  assert.match(prejoin.joinErrorMessage(err("CONFERENCE_NOT_LIVE")), /not started/);
  assert.match(prejoin.joinErrorMessage(err("MEETING_PASSCODE_INVALID")), /passcode/);
  assert.equal(prejoin.joinErrorMessage(err("SOMETHING_ELSE", "Exact backend text")), "Exact backend text");
  assert.equal(prejoin.joinErrorMessage(new Error("network")), "network");
  assert.equal(prejoin.errorCode(err("X")), "X");
});

test("room data comes only from a v2 join response; waiting returns null", () => {
  const response = {
    conference: { id: "c1", meetingSpaceId: "s1", status: "LIVE", isLocked: false },
    participant: { id: "p1", role: "PARTICIPANT", admissionState: "ADMITTED" },
    participantSession: { id: "ps1" }, permissions: { audioPublish: true }, mediaContext: { kind: "MAIN" },
    rtc: { credential: "lk-token", url: "wss://lk", provider: "livekit", expiresAt: "x" }, rtcStatus: "ISSUED",
  };
  assert.deepEqual(plain(prejoin.roomJoinData(response, true)), {
    conferenceId: "c1", meetingSpaceId: "s1", participantId: "p1", participantSessionId: "ps1", conferenceRole: "PARTICIPANT",
    permissions: { audioPublish: true }, mediaContext: { kind: "MAIN" }, isGuest: true, rtcToken: "lk-token", rtcUrl: "wss://lk",
  });
  assert.equal(prejoin.roomJoinData({ ...response, rtc: null, rtcStatus: "NOT_ISSUED_WAITING", participant: { ...response.participant, admissionState: "WAITING" } }, false), null);
  assert.throws(() => prejoin.roomJoinData({ ...response, rtc: null, rtcStatus: "PROVIDER_CONTROL_DISABLED" }, false), /Media access/);
  assert.equal(prejoin.roomPhase("LIVE"), "live");
  assert.equal(prejoin.roomPhase("ENDED"), "closed");
  assert.equal(prejoin.roomPhase("WAITING"), "not-started");
  assert.equal(prejoin.conferenceRoomPath("c 1"), "/conferences/c%201");
  assert.equal(prejoin.conferenceRoomPath("c1", "tok/1"), "/conferences/c1#tok%2F1");
});

test("the web pre-join reads only PreJoinResponse fields the OpenAPI contract declares", () => {
  const generated = readFileSync(new URL("../src/types/generated/meeting-service.ts", import.meta.url), "utf8");
  const block = (name) => generated.match(new RegExp(`export interface ${name} \\{([\\s\\S]*?)\\n\\}`))[1];
  const fields = (name) => [...block(name).matchAll(/^\s{2}(\w+)\??:/gm)].map((m) => m[1]);
  for (const field of ["access", "consents", "notices", "conferenceStatus", "scheduledStartAt", "meetingSpaceId", "displayName"]) assert.ok(fields("PreJoinResponse").includes(field), field);
  for (const field of ["passcodeRequired", "invitationRequired", "authenticationRequired", "waitingRoomExpected", "alreadyAuthorized", "externalAuthorization"]) assert.ok(fields("PreJoinAccess").includes(field), field);
  for (const field of ["required", "granted", "version"]) assert.ok(fields("PreJoinConsents").includes(field), field);
  const screen = readFileSync(new URL("../src/components/room/pre-join-screen.tsx", import.meta.url), "utf8");
  for (const stale of ["consentRequirements", "requiresInvitation", "requiresAuth", "waitingRoomEnabled"]) assert.equal(screen.includes(stale), false, stale);
});

function sources(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? sources(path) : /\.(ts|tsx)$/.test(name) ? [path] : [];
  });
}

test("no legacy v1 meeting, chat or media code is left in the web room (FE-014 / MTG-019)", () => {
  const src = new URL("../src/", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
  for (const removed of ["services/meeting.service.ts", "components/room/ChatProvider.tsx", "hooks/usePublicMeeting.ts", "hooks/useMediaControl.ts", "types/meeting.ts"]) assert.equal(existsSync(join(src, removed)), false, removed);
  // The generated contract still lists the legacy operations the backend serves; no client code may call them.
  const offenders = sources(src).filter((file) => !file.includes("generated")).filter((file) => {
    const text = readFileSync(file, "utf8");
    return /\/api\/v1\/(meetings|participants)|["`']\/meetings\/\$\{[^}]+\}\/chat|["`']\/participants\/\$\{[^}]+\}\/(audio|video)["`']|getInstantJoinData|conference_id|room_name/.test(text);
  });
  assert.deepEqual(offenders, []);
  const room = readFileSync(join(src, "components/room/live-room.tsx"), "utf8");
  assert.match(room, /joinData\.conferenceId/);
  assert.match(room, /LiveKitRoom/);
  assert.ok(existsSync(join(src, "app/(room)/conferences/[conferenceId]/page.tsx")));
});
