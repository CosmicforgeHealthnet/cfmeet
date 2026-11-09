// Section 17 mobile: admission polling, agenda/time display, managed-messaging relay paths,
// error decoding and route mapping.
import { test } from "node:test";
import assert from "node:assert/strict";
import { pollAdmission, waitingPollDelayMs } from "../lib/meeting/admission.ts";
import {
  clock,
  conferenceAction,
  dayKey,
  dayLabel,
  describeTime,
  groupByDay,
  itemsForWorkspace,
  statusLabel,
} from "../lib/meeting/agenda.ts";
import {
  conversationTitle,
  isMobileRelayPath,
  managedConversationBody,
  mergeChatLines,
  relayPaths,
  summarizeConversation,
  toChatLines,
  RelayPathError,
  type ServerMessage,
} from "../lib/messaging/relay-paths.ts";
import { decodeApiError } from "../lib/api/errors.ts";
import { ROUTES, routeTemplate } from "../lib/api/routes.ts";

// ---- FE-005: admission polling ----------------------------------------------------------

function fakeDeps(script: Array<{ state?: string; status?: number; denial?: string }>) {
  const sleeps: number[] = [];
  let calls = 0;
  let aborted = false;
  return {
    sleeps,
    get calls() {
      return calls;
    },
    abort() {
      aborted = true;
    },
    deps: {
      fetchMe: async () => {
        const step = script[Math.min(calls, script.length - 1)];
        calls += 1;
        if (step.status !== undefined) throw Object.assign(new Error("http"), { status: step.status });
        return { admissionState: step.state as never, denialMessage: step.denial ?? null };
      },
      sleep: async (ms: number) => {
        sleeps.push(ms);
      },
      statusOf: (e: unknown) => (e as { status: number }).status,
      isAborted: () => aborted,
    },
  };
}

test("waiting keeps polling the read-only participant state until admitted (no join in the loop)", async () => {
  const f = fakeDeps([{ state: "WAITING" }, { state: "WAITING" }, { state: "ADMITTED" }]);
  const result = await pollAdmission(f.deps);
  assert.deepEqual(result, { kind: "admitted" });
  assert.equal(f.calls, 3);
  assert.deepEqual(f.sleeps, [3000, 3000, 3000]);
});

test("transient failures back off (3 s → 6 s → 12 s) and recover; network errors are not fatal", async () => {
  const f = fakeDeps([{ status: 0 }, { status: 503 }, { status: 0 }, { state: "WAITING" }, { state: "ADMITTED" }]);
  const result = await pollAdmission(f.deps);
  assert.deepEqual(result, { kind: "admitted" });
  assert.deepEqual(f.sleeps, [3000, 6000, 12000, 24000, 3000]);
  assert.equal(waitingPollDelayMs(10), 30000);
});

test("denial stops with the host's message; 401/403/404/410 stop waiting", async () => {
  const denied = await pollAdmission(fakeDeps([{ state: "DENIED", denial: "Please use the clinic link." }]).deps);
  assert.deepEqual(denied, { kind: "refused", reason: "Please use the clinic link." });
  const removed = await pollAdmission(fakeDeps([{ state: "REMOVED" }]).deps);
  assert.equal(removed.kind, "refused");
  for (const status of [401, 403, 404, 410]) {
    assert.deepEqual(await pollAdmission(fakeDeps([{ status }]).deps), { kind: "failed", status });
  }
});

test("leaving the waiting room aborts the poll", async () => {
  const f = fakeDeps([{ state: "WAITING" }]);
  const deps = { ...f.deps, sleep: async () => f.abort() };
  assert.deepEqual(await pollAdmission(deps), { kind: "aborted" });
  assert.equal(f.calls, 0);
});

// ---- Agenda and time display --------------------------------------------------------------

const space = (organizationId: string | null, timezone = "America/New_York") => ({
  accessPolicy: "PUBLIC" as const,
  meetingCode: "abc-defg-hij",
  meetingType: "STANDARD" as const,
  organizationId,
  requiresPasscode: false,
  status: "ACTIVE" as const,
  timezone,
  title: "Standup",
});

test("agenda items follow the workspace: Personal excludes organization meetings", () => {
  const items = [{ meetingSpace: space(null) }, { meetingSpace: space("org-1") }, { meetingSpace: space("org-2") }];
  assert.equal(itemsForWorkspace(items, undefined).length, 1);
  assert.deepEqual(
    itemsForWorkspace(items, "org-1").map((i) => i.meetingSpace.organizationId),
    ["org-1"]
  );
});

test("times show in the device zone, with the space-local time when the zones differ", () => {
  // 14:00Z = 09:00 New York (EST in January) = 14:00 London.
  const t = describeTime("2027-01-15T14:00:00Z", "2027-01-15T15:00:00Z", "America/New_York", "Europe/London");
  assert.equal(t.local, "14:00 – 15:00");
  assert.match(t.spaceLocal ?? "", /^09:00 – 10:00 EST \(meeting time\)$/);
  // Same zone, or zones with the same offset at that instant: no second line.
  assert.equal(describeTime("2027-01-15T14:00:00Z", null, "Europe/London", "Europe/London").spaceLocal, null);
  assert.equal(describeTime("2027-01-15T14:00:00Z", null, "Europe/Lisbon", "Europe/London").spaceLocal, null);
  assert.equal(describeTime("2027-01-15T14:00:00Z", null, null, "Asia/Tokyo").local, "23:00");
});

test("days group in the display zone (a late-evening UTC meeting is 'tomorrow' in Tokyo)", () => {
  const items = [
    { scheduledStartAt: "2027-03-01T23:30:00Z" },
    { scheduledStartAt: "2027-03-01T09:00:00Z" },
    { scheduledStartAt: "2027-03-02T01:00:00Z" },
  ];
  const utc = groupByDay(items, "UTC");
  assert.deepEqual(utc.map((g) => [g.day, g.items.length]), [["2027-03-01", 2], ["2027-03-02", 1]]);
  const tokyo = groupByDay(items, "Asia/Tokyo");
  assert.deepEqual(tokyo.map((g) => [g.day, g.items.length]), [["2027-03-01", 1], ["2027-03-02", 2]]);
  assert.equal(tokyo[1].items[0].scheduledStartAt, "2027-03-01T23:30:00Z");
});

test("day labels: Today / Tomorrow / weekday date", () => {
  const now = Date.parse("2027-03-01T10:00:00Z");
  assert.equal(dayLabel(dayKey(now, "UTC"), now, "UTC"), "Today");
  assert.equal(dayLabel("2027-03-02", now, "UTC"), "Tomorrow");
  assert.equal(dayLabel("2027-03-05", now, "UTC", "en-US"), "Fri, Mar 5");
  assert.equal(clock(Date.parse("2027-07-01T12:05:00Z"), "Europe/Paris"), "14:05");
});

test("conference actions: hosts start a scheduled session, others join; ended sessions offer nothing", () => {
  assert.equal(conferenceAction("CREATED", "OWNER"), "start");
  assert.equal(conferenceAction("CREATED", "PARTICIPANT"), "join");
  assert.equal(conferenceAction("LIVE", "VIEWER"), "join");
  assert.equal(conferenceAction("ENDED", "OWNER"), "none");
  assert.equal(conferenceAction("CANCELLED", null), "none");
  assert.equal(statusLabel("WAITING"), "Scheduled");
});

// ---- Managed messaging relay ----------------------------------------------------------------

const CID = "0f8fad5b-d9cb-469f-a165-70867728950e";

test("relay paths are allowlisted, ids validated, query keys restricted", () => {
  assert.equal(relayPaths.messagesAfter(CID, 12), `v1/conversations/${CID}/messages?afterSequence=12&limit=50`);
  assert.ok(isMobileRelayPath("GET", relayPaths.conversations("abc")));
  assert.ok(isMobileRelayPath("POST", relayPaths.sendMessage(CID)));
  assert.ok(isMobileRelayPath("PUT", relayPaths.readCursor(CID)));
  assert.ok(!isMobileRelayPath("POST", `v1/conversations/${CID}/mls/commits`), "MLS operations are web-only");
  assert.ok(!isMobileRelayPath("GET", `v1/conversations/${CID}/messages?evil=1`));
  assert.ok(!isMobileRelayPath("DELETE", `v1/conversations/${CID}/messages/${CID}`));
  assert.throws(() => relayPaths.conversation("../admin"), RelayPathError);
  assert.throws(() => relayPaths.messagesAfter(CID, -1), RelayPathError);
});

test("relay routes map to the contract's /messaging/relay/{path} without double encoding", () => {
  assert.equal(routeTemplate(ROUTES.relayGet as never), "/api/v2/messaging/relay/{}");
  assert.equal(ROUTES.relayGet.path(relayPaths.conversation(CID)), `/messaging/relay/v1/conversations/${CID}`);
  assert.equal(routeTemplate(ROUTES.organizationMembers as never), "/api/v2/organizations/{}/members");
  assert.equal(routeTemplate(ROUTES.myConferences as never), "/api/v2/me/conferences");
});

test("only MANAGED application messages become chat lines; private content is never shown", () => {
  const base = { conversationId: CID, senderPrincipalId: "p1", clientMessageId: "c", createdAt: "2027-01-01T00:00:00Z" };
  const messages: ServerMessage[] = [
    { ...base, messageId: "m2", sequence: 2, kind: "APPLICATION", securityProfile: "MANAGED", lifecycle: "ACTIVE", managed: { text: "hi" } },
    { ...base, messageId: "m1", sequence: 1, kind: "MLS_COMMIT", securityProfile: "MANAGED", lifecycle: "ACTIVE" },
    { ...base, messageId: "m3", sequence: 3, kind: "APPLICATION", securityProfile: "PRIVATE_E2EE", lifecycle: "ACTIVE" },
    { ...base, messageId: "m4", sequence: 4, kind: "APPLICATION", securityProfile: "MANAGED", lifecycle: "DELETED", managed: { text: "secret" } },
  ];
  const lines = toChatLines(messages, "p1");
  assert.deepEqual(lines.map((l) => [l.sequence, l.text, l.removed, l.mine]), [[2, "hi", false, true], [4, null, true, true]]);
  const merged = mergeChatLines(lines, toChatLines([messages[0]], "p1"));
  assert.equal(merged.length, 2);
});

test("conversation summaries are truthful about the security profile", () => {
  const priv = summarizeConversation({
    conversationId: CID,
    contextClass: "DIRECT",
    securityProfile: "PRIVATE_E2EE",
    lifecycleState: "ACTIVE",
    createdAt: "",
    members: [{ principalId: "me" }, { principalId: "p2" }],
  });
  assert.equal(priv.readableHere, false);
  assert.equal(conversationTitle(priv, "me", { p2: "Ada" }), "Ada");
  assert.deepEqual(managedConversationBody(["p2"]), {
    contextClass: "DIRECT",
    counterpartPrincipalId: "p2",
    securityProfile: "MANAGED",
    managedDisclosureAcknowledged: true,
  });
  assert.equal((managedConversationBody(["a", "b"]) as { contextClass: string }).contextClass, "GROUP");
  assert.throws(() => managedConversationBody([]), RelayPathError);
});

// ---- FE-019: error decoding -----------------------------------------------------------------

function httpError(status: number, data: unknown, url = "/conferences/x/join", bearer = true) {
  return {
    isAxiosError: true,
    config: { url, headers: bearer ? { Authorization: "Bearer t" } : {} },
    response: { status, data },
  };
}

test("v2 envelope: semantic codes classify independently of the HTTP status", () => {
  const notLive = decodeApiError(httpError(409, { success: false, error: { code: "CONFERENCE_NOT_LIVE", message: "This conference has not started yet", requestId: "r-1" } }));
  assert.equal(notLive.kind, "conflict");
  assert.equal(notLive.code, "CONFERENCE_NOT_LIVE");
  assert.equal(notLive.message, "This conference has not started yet");
  assert.equal(notLive.requestId, "r-1");
  const forbidden = decodeApiError(httpError(403, { error: { code: "ORGANIZATION_POLICY_RESTRICTED", message: "Restricted" } }));
  assert.equal(forbidden.kind, "forbidden");
  const limited = decodeApiError(httpError(429, { error: { code: "RATE_LIMITED", message: "Slow down", retryAfterSeconds: 7 } }));
  assert.equal(limited.kind, "rate_limited");
  assert.equal(limited.retryAfterSeconds, 7);
  const guest = decodeApiError(httpError(401, { error: { code: "AUTHENTICATION_REQUIRED", message: "x" } }, "/conferences/x/participants/me", false));
  assert.equal(guest.kind, "auth_required", "a guest's 401 is not an 'expired session'");
  const offline = decodeApiError({ isAxiosError: true, config: { url: "/x" } });
  assert.equal(offline.kind, "network");
});
