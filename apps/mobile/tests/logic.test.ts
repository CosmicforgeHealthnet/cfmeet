import { test } from "node:test";
import assert from "node:assert/strict";
import { resolveApiUrl, insecureApiUrlReason, toV2BaseUrl, parseAbsoluteUrl } from "../lib/config/api-url.ts";
import { decodeApiError, classify } from "../lib/api/errors.ts";
import { classifyAccountStatus, routeAfterSignIn } from "../lib/auth/account-status.ts";
import { zonedToUtc, toIsoUtc, wallTimeAt, scheduleProblem } from "../lib/time/zoned.ts";
import { effectiveRole, meetingAllows, mayTarget } from "../lib/meeting/authority.ts";
import {
  classifyJoin,
  nextWaitingStep,
  waitingPollDelayMs,
  isFatalPollFailure,
} from "../lib/meeting/admission.ts";
import { reconcileWorkspace, organizationIdOf, isContextKey, contextKey } from "../lib/workspace/context.ts";
import { parseInvitationToken, parseMeetingIdentifier } from "../lib/invitations.ts";

// ---- FE-006: API URL -------------------------------------------------------------------
test("release builds refuse cleartext HTTP to a LAN host and fall back to HTTPS", () => {
  const r = resolveApiUrl({ envUrl: "http://192.168.1.20:8080/api/v1", isDev: false });
  assert.equal(r.apiUrl, "https://api.cfmeet.com/api/v1");
  assert.equal(r.source, "default");
  assert.match(r.rejected!.reason, /cleartext/);
});

test("development builds may use a LAN host; loopback allowed everywhere", () => {
  assert.equal(resolveApiUrl({ envUrl: "http://192.168.1.20:8080/api/v1/", isDev: true }).apiUrl, "http://192.168.1.20:8080/api/v1");
  assert.equal(insecureApiUrlReason("http://localhost:8080/api/v1", false), null);
  assert.equal(insecureApiUrlReason("http://10.0.2.2:8080/api/v1", false), null);
});

test("URLs with credentials or odd protocols are refused", () => {
  assert.match(insecureApiUrlReason("https://u:p@api.example.com/api/v1", true)!, /credentials/);
  assert.match(insecureApiUrlReason("ftp://api.example.com", true)!, /protocol/);
  assert.match(insecureApiUrlReason("not a url", true)!, /valid/);
});

test("v2 base pairs with v1 base; parser handles ports and IPv6", () => {
  assert.equal(toV2BaseUrl("https://x.test/api/v1"), "https://x.test/api/v2");
  assert.equal(parseAbsoluteUrl("https://[::1]:8443/api/v1?x=1")!.hostname, "[::1]");
  assert.equal(parseAbsoluteUrl("HTTPS://API.Example.com:443/a")!.host, "api.example.com:443");
  assert.equal(parseAbsoluteUrl("https://h:99999/"), null);
});

// ---- FE-019: errors ------------------------------------------------------------------------
const axiosError = (status: number, data: unknown, url = "/conferences/x/join", bearer = true) => ({
  isAxiosError: true,
  config: { url, headers: bearer ? { Authorization: "Bearer abc" } : {} },
  response: { status, data },
});

test("v2 envelope is decoded with code and requestId", () => {
  const e = decodeApiError(
    axiosError(409, { success: false, error: { code: "CONFERENCE_NOT_LIVE", message: "Not live", requestId: "r1" } })
  );
  assert.equal(e.code, "CONFERENCE_NOT_LIVE");
  assert.equal(e.requestId, "r1");
  assert.equal(e.kind, "conflict");
  assert.equal(e.message, "Not live");
});

test("a passcode 401 is a validation error, not an expired session", () => {
  const e = decodeApiError(axiosError(401, { error: { code: "MEETING_PASSCODE_REQUIRED", message: "Passcode required" } }));
  assert.equal(e.kind, "validation");
  assert.equal(e.message, "Passcode required");
});

test("401 without a bearer token is 'auth required', with one 'session expired', on login 'invalid credentials'", () => {
  assert.equal(decodeApiError(axiosError(401, { error: { code: "AUTHENTICATION_REQUIRED" } }, "/x", false)).kind, "auth_required");
  assert.equal(decodeApiError(axiosError(401, { error: { code: "AUTHENTICATION_REQUIRED" } }, "/x", true)).kind, "auth_expired");
  assert.equal(decodeApiError(axiosError(401, { error: { code: "UNAUTHORIZED" } }, "/auth/login", false)).kind, "invalid_credentials");
});

test("verification-required and network errors are classified", () => {
  assert.equal(classify(403, "EMAIL_VERIFICATION_REQUIRED", "/auth/login"), "verification_required");
  assert.equal(decodeApiError({ isAxiosError: true, config: {}, response: undefined }).kind, "network");
});

// ---- ACC-012: account status --------------------------------------------------------------
test("both spellings of the unverified status route to verification with the typed email", () => {
  assert.equal(classifyAccountStatus("pending_verification"), "pending_verification");
  assert.equal(classifyAccountStatus("pendingverification"), "pending_verification");
  assert.deepEqual(routeAfterSignIn("pendingverification", " Me@Example.com "), { kind: "verify", email: "me@example.com" });
  assert.deepEqual(routeAfterSignIn("active", "a@b.c"), { kind: "home" });
  assert.deepEqual(routeAfterSignIn("suspended", "a@b.c"), { kind: "blocked" });
});

// ---- MTG-012: time zones -----------------------------------------------------------------
test("wall time in the space zone converts to UTC, independent of the device zone", () => {
  assert.equal(toIsoUtc(zonedToUtc({ year: 2026, month: 11, day: 20, hour: 9, minute: 0 }, "Africa/Lagos")), "2026-11-20T08:00:00Z");
  assert.equal(toIsoUtc(zonedToUtc({ year: 2026, month: 7, day: 1, hour: 9, minute: 0 }, "America/New_York")), "2026-07-01T13:00:00Z");
  assert.equal(toIsoUtc(zonedToUtc({ year: 2026, month: 1, day: 1, hour: 0, minute: 0 }, "UTC")), "2026-01-01T00:00:00Z");
});

test("spring-forward gap shifts forward; fall-back overlap takes the first instant", () => {
  // 2026-03-08 02:30 does not exist in New York → 03:30 EDT = 07:30Z
  assert.equal(toIsoUtc(zonedToUtc({ year: 2026, month: 3, day: 8, hour: 2, minute: 30 }, "America/New_York")), "2026-03-08T07:30:00Z");
  // 2026-11-01 01:30 occurs twice → first (EDT) = 05:30Z
  assert.equal(toIsoUtc(zonedToUtc({ year: 2026, month: 11, day: 1, hour: 1, minute: 30 }, "America/New_York")), "2026-11-01T05:30:00Z");
  const w = wallTimeAt(Date.parse("2026-11-01T05:30:00Z"), "America/New_York");
  assert.deepEqual([w.hour, w.minute], [1, 30]);
});

test("schedule validation mirrors the backend", () => {
  const now = Date.parse("2026-11-08T10:00:00Z");
  assert.equal(scheduleProblem(now - 4 * 60000, null, now), null);
  assert.match(scheduleProblem(now - 6 * 60000, null, now)!, /past/);
  assert.match(scheduleProblem(now + 60000, now, now)!, /after/);
  assert.match(scheduleProblem(now, now + 25 * 3600000, now)!, /24 hours/);
});

// ---- server-permitted controls --------------------------------------------------------------
test("controls follow the effective role; removed participants get none", () => {
  assert.equal(effectiveRole("HOST", "PARTICIPANT"), "HOST");
  assert.equal(effectiveRole(undefined, "CO_HOST"), "CO_HOST");
  assert.equal(effectiveRole("HOST", "HOST", "REMOVED"), undefined);
  assert.equal(meetingAllows("PARTICIPANT", "participant.admit"), false);
  assert.equal(meetingAllows("CO_HOST", "participant.admit"), true);
  assert.equal(meetingAllows("CO_HOST", "conference.end"), false);
  assert.equal(meetingAllows("MODERATOR", "conference.lock"), false);
  assert.equal(mayTarget("HOST", "OWNER", false), false);
  assert.equal(mayTarget("HOST", "PARTICIPANT", true), false);
  assert.equal(mayTarget("CO_HOST", "PARTICIPANT", false), true);
});

// ---- FE-005 / MTG-002: admission ------------------------------------------------------------
const base = {
  conference: { id: "c1", isLocked: false, meetingSpaceId: "s1", status: "LIVE" as const },
  mediaContext: { kind: "MAIN", visit: false },
  permissions: { audioPublish: true, dataPublish: true, screenSharePublish: false, subscribe: true, videoPublish: true },
  policy: { recordingActive: false, transcriptionActive: true },
  realtime: { conferencePath: "/x", enabled: false, participantPath: "/y" },
};
const participant = (admissionState: "WAITING" | "ADMITTED" | "DENIED", denialMessage?: string) => ({
  admissionState,
  conferenceId: "c1",
  displayName: "Guest",
  id: "p1",
  identityType: "GUEST" as const,
  requestedAt: "2026-11-08T10:00:00Z",
  role: "PARTICIPANT" as const,
  version: 1,
  denialMessage,
});

test("an issued credential goes live with the server's permissions", () => {
  const o = classifyJoin({
    ...base,
    participant: participant("ADMITTED"),
    participantSession: { id: "sess1" },
    rtc: { credential: "jwt", expiresAt: "2026-11-08T11:00:00Z", provider: "livekit", url: "wss://media.test" },
    rtcStatus: "ISSUED",
  });
  assert.equal(o.kind, "live");
  if (o.kind === "live") {
    assert.equal(o.media.conferenceId, "c1");
    assert.equal(o.media.participantSessionId, "sess1");
    assert.equal(o.media.permissions.screenSharePublish, false);
    assert.equal(o.media.transcriptionActive, true);
  }
});

test("waiting never carries media; denied shows the server's message", () => {
  assert.equal(classifyJoin({ ...base, participant: participant("WAITING"), rtcStatus: "NOT_ISSUED_WAITING" }).kind, "waiting");
  const denied = classifyJoin({ ...base, participant: participant("DENIED", "Not today"), rtcStatus: "NOT_ISSUED_WAITING" });
  assert.deepEqual(denied, { kind: "refused", reason: "Not today" });
  assert.equal(classifyJoin({ ...base, participant: participant("ADMITTED"), rtcStatus: "PROVIDER_CONTROL_DISABLED" }).kind, "media_unavailable");
});

test("waiting polls read admission state and back off on failures", () => {
  assert.deepEqual(nextWaitingStep({ admissionState: "WAITING" }), { kind: "keep_waiting" });
  assert.deepEqual(nextWaitingStep({ admissionState: "ADMITTED" }), { kind: "admitted" });
  assert.equal(nextWaitingStep({ admissionState: "BANNED" }).kind, "refused");
  assert.equal(waitingPollDelayMs(0), 3000);
  assert.equal(waitingPollDelayMs(10), 30000);
  assert.equal(isFatalPollFailure(0), false);
  assert.equal(isFatalPollFailure(503), false);
  assert.equal(isFatalPollFailure(404), true);
});

// ---- ORG-016: workspace context ---------------------------------------------------------------
test("a removed organization context falls back to Personal and is reported for cache purge", () => {
  const ws = [{ id: "personal" }, { id: "org-a" }];
  assert.deepEqual(reconcileWorkspace("org-a", ws), { workspaceId: "org-a", removedWorkspaceId: null });
  assert.deepEqual(reconcileWorkspace("org-b", ws), { workspaceId: "personal", removedWorkspaceId: "org-b" });
  assert.deepEqual(reconcileWorkspace(null, ws), { workspaceId: "personal", removedWorkspaceId: null });
  assert.equal(organizationIdOf({ kind: "ORGANIZATION", organizationId: "org-a" }), "org-a");
  assert.equal(organizationIdOf({ kind: "PERSONAL" }), undefined);
  assert.equal(isContextKey([...contextKey("org-a"), "agenda"], "org-a"), true);
  assert.equal(isContextKey(["profile"], "org-a"), false);
});

// ---- invitations and meeting codes -------------------------------------------------------------
test("invitation tokens are taken from links, deep links or bare tokens", () => {
  const token = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789_-abcde";
  assert.equal(parseInvitationToken(`https://app.cfmeet.com/organizations/invitations#${token}`), token);
  assert.equal(parseInvitationToken(`cfmeet://organizations/invitations?token=${token}`), token);
  assert.equal(parseInvitationToken(` ${token} `), token);
  assert.equal(parseInvitationToken("short"), null);
  assert.equal(parseInvitationToken("bad token with spaces and more text"), null);
});

test("meeting codes are taken from codes or links", () => {
  assert.equal(parseMeetingIdentifier("abc-defg-hij"), "abc-defg-hij");
  assert.equal(parseMeetingIdentifier("https://app.cfmeet.com/meeting/abc-defg-hij?x=1"), "abc-defg-hij");
  assert.equal(parseMeetingIdentifier("a b"), null);
});
