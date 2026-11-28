// Events (ADR 0071, Messaging): form → request bodies (local time in an IANA zone → UTC with
// the backend's DST rules, all-day = local midnights), recurrence text, the month grid, list
// queries, cursor paging, event links in chats and the relay allowlist entries.
// Run: node --experimental-strip-types --test tests/events.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import { isMessagingRelayRoute } from "../src/lib/messaging/routes.ts";

function load(path, dependencies = {}) {
  const exports = {};
  const source = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  vm.runInNewContext(source, { exports, Date, Intl, Map, Set, Error, JSON, Math, Number, String, RegExp, URL, URLSearchParams, Object, Array, require: (name) => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency: ${name}`);
  } });
  return exports;
}

const time = load("../src/lib/meeting-time.ts");
// Values made in the module's own realm are cloned into this one (strict deep equality).
const core = Object.fromEntries(Object.entries(load("../src/lib/events/core.ts", { "../meeting-time": time }))
  .map(([k, v]) => [k, typeof v === "function" ? (...args) => structuredClone(v(...args)) : structuredClone(v)]));
const ID = "11111111-1111-4111-8111-111111111111";
const C = "22222222-2222-4222-8222-222222222222";
const P = "33333333-3333-4333-8333-333333333333";
const NOW = Date.parse("2026-01-01T00:00:00Z");

const form = (over = {}) => ({ ...core.emptyForm("Europe/London", NOW), title: "Dinner", ...over });

test("a timed event's local date and time in its zone become UTC instants", () => {
  const body = core.buildCreateBody(form({ startDate: "2026-07-10", startTime: "19:00", endDate: "2026-07-10", endTime: "21:30" }));
  assert.equal(body.startsAt, "2026-07-10T18:00:00.000Z"); // BST
  assert.equal(body.endsAt, "2026-07-10T20:30:00.000Z");
  assert.equal(body.timeZone, "Europe/London");
  assert.equal(body.allDay, false);
  assert.equal(body.scope, "PERSONAL");
  assert.equal("conversationId" in body, false);
  assert.deepEqual(body.remindersMinutesBefore, [30]);
  const winter = core.eventTimes({ startDate: "2026-12-10", startTime: "19:00", endDate: "2026-12-10", endTime: "20:00", allDay: false, timeZone: "Europe/London" });
  assert.equal(winter.startsAt, "2026-12-10T19:00:00.000Z");
});

test("daylight saving: a skipped local time moves forward, a repeated one takes the first instant", () => {
  const ny = (startTime) => core.eventTimes({ startDate: "2026-03-08", startTime, endDate: "2026-03-08", endTime: "05:00", allDay: false, timeZone: "America/New_York" });
  assert.equal(ny("02:30").startsAt, "2026-03-08T07:30:00.000Z"); // 03:30 EDT
  const back = core.eventTimes({ startDate: "2026-11-01", startTime: "01:30", endDate: "2026-11-01", endTime: "03:00", allDay: false, timeZone: "America/New_York" });
  assert.equal(back.startsAt, "2026-11-01T05:30:00.000Z"); // first 01:30 (EDT)
});

test("all day: the local midnight of the first day to the midnight after the last day", () => {
  const body = core.buildCreateBody(form({ allDay: true, startDate: "2026-03-28", endDate: "2026-03-29", timeZone: "Europe/London" }));
  assert.equal(body.startsAt, "2026-03-28T00:00:00.000Z");
  assert.equal(body.endsAt, "2026-03-29T23:00:00.000Z"); // 30 Mar 00:00 BST (clocks changed on the 29th)
  assert.equal(body.allDay, true);
  const lagos = core.eventTimes({ allDay: true, startDate: "2026-05-01", endDate: "2026-05-01", startTime: "", endTime: "", timeZone: "Africa/Lagos" });
  assert.deepEqual(lagos, { startsAt: "2026-04-30T23:00:00.000Z", endsAt: "2026-05-01T23:00:00.000Z" });
  // Round trip: the form of an all-day event shows its last day, not the day after.
  const back = core.formFromEvent({ ...event(), allDay: true, timeZone: "Africa/Lagos", startsAt: lagos.startsAt, endsAt: lagos.endsAt });
  assert.equal(back.startDate, "2026-05-01");
  assert.equal(back.endDate, "2026-05-01");
});

test("the create body carries scope, chat, meeting, recurrence and people", () => {
  const body = core.buildCreateBody(form({
    scope: "CONVERSATION", conversationId: C, type: "ONLINE", meetingId: "space_1", meetingUrl: "https://meet.example/abc-defg-hij",
    description: "  bring snacks ", location: "", startDate: "2026-07-06", endDate: "2026-07-06",
    frequency: "WEEKLY", interval: 2, byDay: ["WE", "MO"], ends: "COUNT", count: 10,
    coOrganizerPrincipalIds: [P, P], inviteePrincipalIds: [P, ID], reminders: [60, 10, 60],
  }));
  assert.equal(body.conversationId, C);
  assert.equal(body.description, "bring snacks");
  assert.equal("location" in body, false);
  assert.equal(body.meetingId, "space_1");
  assert.deepEqual(body.recurrence, { frequency: "WEEKLY", interval: 2, byDay: ["MO", "WE"], count: 10 });
  assert.deepEqual(body.coOrganizerPrincipalIds, [P]);
  assert.deepEqual(body.inviteePrincipalIds, [ID]);
  assert.deepEqual(body.remindersMinutesBefore, [10, 60]);
  // A meeting is only sent for an online event.
  assert.equal("meetingId" in core.buildCreateBody(form({ type: "PHYSICAL", meetingId: "x" })), false);
});

test("form validation names the problem", () => {
  const ok = form({ startDate: "2026-07-06", endDate: "2026-07-06" });
  assert.equal(core.validateForm(ok, NOW), null);
  assert.match(core.validateForm({ ...ok, title: " " }, NOW), /title/);
  assert.match(core.validateForm({ ...ok, type: "ONLINE" }, NOW), /meeting/);
  assert.match(core.validateForm({ ...ok, type: "ONLINE", meetingUrl: "http://x" }, NOW), /https/);
  assert.match(core.validateForm({ ...ok, endTime: ok.startTime }, NOW), /after the start/);
  assert.match(core.validateForm({ ...ok, endDate: "2026-08-10" }, NOW), /31 days/);
  assert.match(core.validateForm({ ...ok, startDate: "2025-07-06", endDate: "2025-07-06" }, NOW), /passed/);
  // 2026-07-06 is a Monday: a weekly series must include it.
  assert.match(core.validateForm({ ...ok, frequency: "WEEKLY", byDay: ["TU"] }, NOW), /Mon/);
  assert.equal(core.validateForm({ ...ok, frequency: "WEEKLY", byDay: ["TU", "MO"] }, NOW), null);
  assert.match(core.validateForm({ ...ok, frequency: "DAILY", ends: "COUNT", count: 731 }, NOW), /730/);
  assert.match(core.validateForm({ ...ok, frequency: "DAILY", ends: "UNTIL", until: "2026-07-01" }, NOW), /before it starts/);
  assert.match(core.validateForm({ ...ok, scope: "CONVERSATION", conversationId: "" }, NOW), /chat/);
});

function event(over = {}) {
  return {
    eventId: ID, scope: "PERSONAL", conversationId: null, title: "Dinner", description: null, location: "Home", type: "PHYSICAL",
    meetingId: null, meetingUrl: null, coverMediaId: null, startsAt: "2026-07-10T18:00:00Z", endsAt: "2026-07-10T20:00:00Z",
    timeZone: "Europe/London", allDay: false, recurrence: null, rrule: null, seriesEndsAt: null, state: "ACTIVE", cancelledAt: null,
    cancelReason: null, organizerPrincipalId: P, coOrganizerPrincipalIds: [], myRole: "ORGANIZER", myResponse: null, myReminders: [30],
    counts: { going: 0, maybe: 0, notGoing: 0, awaiting: 0 }, canEdit: true, canDelete: true, version: 4, createdAt: "", updatedAt: "",
    nextOccurrence: null, ...over,
  };
}

test("the patch sends only what changed, null clears, and always the version", () => {
  const original = event();
  const unchanged = core.buildPatchBody(core.formFromEvent(original), original);
  assert.deepEqual(unchanged, { version: 4 });
  const edited = { ...core.formFromEvent(original), location: "", startTime: "20:00", endTime: "22:00", frequency: "MONTHLY", ends: "UNTIL", until: "2026-12-31" };
  assert.deepEqual(core.buildPatchBody(edited, original), {
    version: 4, location: null, startsAt: "2026-07-10T19:00:00.000Z", endsAt: "2026-07-10T21:00:00.000Z",
    recurrence: { frequency: "MONTHLY", interval: 1, until: "2026-12-31" },
  });
});

test("recurrence summaries", () => {
  assert.equal(core.recurrenceSummary(null), "Does not repeat");
  assert.equal(core.recurrenceSummary({ frequency: "DAILY", interval: 1 }), "Every day");
  assert.equal(core.recurrenceSummary({ frequency: "DAILY", interval: 3 }), "Every 3 days");
  assert.equal(core.recurrenceSummary({ frequency: "WEEKLY", interval: 2, byDay: ["WE", "MO"], count: 10 }), "Every 2 weeks on Mon, Wed, 10 times");
  assert.equal(core.recurrenceSummary({ frequency: "MONTHLY", interval: 1, until: "2026-12-31" }, "2026-07-15"), "Every month on day 15, until 31 Dec 2026");
  assert.equal(core.recurrenceSummary({ frequency: "WEEKLY", byDay: core.WEEKDAYS, count: 1 }), "Every week on every day, once");
  assert.equal(core.reminderLabel(0), "At start of event");
  assert.equal(core.reminderLabel(10), "10 minutes before");
  assert.equal(core.reminderLabel(60), "1 hour before");
  assert.equal(core.reminderLabel(2880), "2 days before");
  assert.equal(core.reminderLabel(10080), "1 week before");
});

test("the month grid covers whole weeks, Monday first, and its window is under 92 days", () => {
  const weeks = core.monthGrid("2026-11");
  assert.equal(weeks[0][0].date, "2026-10-26");
  assert.equal(weeks[0][0].inMonth, false);
  assert.equal(weeks.at(-1)[6].date, "2026-12-06");
  assert.ok(weeks.every((w) => w.length === 7));
  assert.equal(weeks.length, 6);
  assert.equal(core.monthGrid("2027-02").length, 4); // Feb 2027 starts on a Monday, 28 days
  assert.equal(core.monthGrid("2026-11", 0)[0][0].date, "2026-11-01"); // Sunday first
  const range = core.monthRange("2026-11", "Europe/London");
  assert.deepEqual(range, { from: "2026-10-26T00:00:00.000Z", to: "2026-12-07T00:00:00.000Z" });
  assert.ok((Date.parse(range.to) - Date.parse(range.from)) / 86_400_000 <= core.LIMITS.calendarDays);
});

const item = (startsAt, endsAt, over = {}) => ({ startsAt, endsAt, event: event({ startsAt, endsAt, ...over }) });

test("occurrences are grouped by the viewer's local day; all-day events keep their own dates", () => {
  const a = item("2026-07-10T22:30:00Z", "2026-07-10T23:30:00Z", { eventId: ID });
  const b = item("2026-07-11T08:00:00Z", "2026-07-11T09:00:00Z", { eventId: C });
  assert.deepEqual(core.groupByDay([a, b], "Africa/Lagos").map((g) => [g.date, g.items.length]), [["2026-07-10", 1], ["2026-07-11", 1]]);
  assert.deepEqual(core.groupByDay([a, b], "Asia/Tokyo").map((g) => [g.date, g.items.length]), [["2026-07-11", 2]]);
  // An all-day event in Lagos stays on its date for a viewer in Los Angeles.
  const allDay = item("2026-04-30T23:00:00Z", "2026-05-02T23:00:00Z", { allDay: true, timeZone: "Africa/Lagos" });
  assert.deepEqual(core.occurrenceDates(allDay, "America/Los_Angeles"), ["2026-05-01", "2026-05-02"]);
  const byDate = core.occurrencesByDate([b, allDay], "UTC");
  assert.equal(byDate.get("2026-05-01").length, 1);
  // Ending exactly at midnight doesn't spill into the next day.
  assert.deepEqual(core.occurrenceDates(item("2026-07-10T22:00:00Z", "2026-07-11T00:00:00Z"), "UTC"), ["2026-07-10"]);
});

test("list queries keep the relay's key order and drop empty values", () => {
  assert.equal(core.listQuery({ view: "UPCOMING" }), "view=UPCOMING&limit=30");
  // TODAY is the caller's day in their zone, sent as from / to (Lagos is UTC+1).
  const noon = Date.parse("2026-11-28T12:00:00Z");
  assert.equal(core.listQuery({ view: "TODAY", tz: "Africa/Lagos", q: "  din ", type: "CALL", conversationId: C, cursor: "abc+=" }, noon),
    `view=TODAY&from=2026-11-27T23%3A00%3A00.000Z&to=2026-11-28T23%3A00%3A00.000Z&conversationId=${C}&q=din&type=CALL&limit=30&cursor=abc%2B%3D`);
  // A calendar window is absolute.
  assert.equal(core.listQuery({ view: "UPCOMING", tz: "UTC", from: "2026-10-26T00:00:00.000Z", to: "2026-12-07T00:00:00.000Z", limit: 500 }),
    "view=UPCOMING&from=2026-10-26T00%3A00%3A00.000Z&to=2026-12-07T00%3A00%3A00.000Z&limit=100");
  assert.deepEqual([...core.LIST_QUERY_KEYS], ["view", "from", "to", "conversationId", "q", "type", "limit", "cursor"]);
  for (const view of core.EVENT_VIEWS.map((v) => v.value)) {
    const q = core.listQuery({ view, tz: "UTC", conversationId: C, q: "x", type: "ONLINE", cursor: "c" });
    assert.equal(isMessagingRelayRoute("GET", `v1/messaging/events?${q}`), true, q);
  }
});

test("pages merge in order without an occurrence twice", () => {
  const a = item("2026-07-10T18:00:00Z", "2026-07-10T19:00:00Z");
  const b = item("2026-07-17T18:00:00Z", "2026-07-17T19:00:00Z");
  const merged = core.mergeEventPages([{ items: [a, b] }, { items: [{ ...b, startsAt: "2026-07-17T18:00:00.000Z" }, item("2026-07-24T18:00:00Z", "2026-07-24T19:00:00Z")] }]);
  assert.equal(merged.length, 3);
  assert.deepEqual(core.mergeEventPages(undefined), []);
});

test("event links: the page, the shared text, and links found in chat messages", () => {
  const origin = "https://cfmeet.example";
  assert.equal(core.eventHref(ID.toUpperCase()), `/dashboard/events/${ID}`);
  assert.throws(() => core.eventHref("../x"));
  assert.equal(core.eventShareText(origin, { eventId: ID, title: " Dinner " }), `Event: Dinner\n${origin}/dashboard/events/${ID}`);
  assert.deepEqual(core.eventIdsInText(`see ${origin}/dashboard/events/${ID} and ${origin}/dashboard/events/${ID}!`, origin), [ID]);
  assert.deepEqual(core.eventIdsInText(`https://evil.example/dashboard/events/${ID}`, origin), []);
  assert.deepEqual(core.eventIdsInText(`${origin}/dashboard/events/${ID}x`, origin), []);
  assert.deepEqual(core.eventIdsInText(null, origin), []);
  assert.deepEqual(core.meetingTarget({ meetingUrl: `${origin}/abc-defg-hij`, meetingId: null }, origin), { href: "/abc-defg-hij", external: false });
  assert.deepEqual(core.meetingTarget({ meetingUrl: null, meetingId: "space-1" }, origin), { href: "/dashboard/meetings/space-1", external: false });
  assert.equal(core.meetingTarget({ meetingUrl: "javascript:alert(1)", meetingId: null }, origin), null);
  assert.equal(core.meetingJoinUrl(origin, "abc-defg-hij"), `${origin}/abc-defg-hij`);
  assert.equal(core.meetingJoinUrl("http://localhost:3000", "abc-defg-hij"), null);
  assert.equal(core.icsFilename("Team dinner.ics"), "Team-dinner.ics");
  assert.equal(core.icsFilename("../../x"), "x.ics");
  assert.equal(core.icsFilename(""), "event.ics");
});

test("the relay allowlist carries every events operation, and nothing else", () => {
  const e = "v1/messaging/events";
  const allowed = [
    ["GET", `${e}?view=UPCOMING&from=a&to=b&conversationId=${C}&q=x&type=CALL&limit=30`],
    ["GET", `${e}?view=PAST&cursor=abc`],
    ["POST", e], ["GET", `${e}/calendar`],
    ["GET", `${e}/${ID}`], ["PATCH", `${e}/${ID}`], ["DELETE", `${e}/${ID}`],
    ["POST", `${e}/${ID}/cancel`], ["PUT", `${e}/${ID}/rsvp`], ["GET", `${e}/${ID}/attendees`],
    ["POST", `${e}/${ID}/invitations`], ["DELETE", `${e}/${ID}/invitations/${P}`],
    ["PUT", `${e}/${ID}/reminders`], ["GET", `${e}/${ID}/ics`],
    ["POST", `${e}/media`], ["GET", `${e}/media/${ID}`], ["DELETE", `${e}/media/${ID}`],
    ["POST", `${e}/media/${ID}/parts/0`], ["POST", `${e}/media/${ID}/parts/63`],
    ["POST", `${e}/media/${ID}/complete`], ["POST", `${e}/media/${ID}/download-grants`],
  ];
  for (const [method, path] of allowed) assert.equal(isMessagingRelayRoute(method, path), true, `${method} ${path}`);
  const refused = [
    ["GET", `${e}?organizationId=${C}`], ["GET", `${e}/calendar?view=PAST`], ["PUT", `${e}/${ID}`],
    ["POST", `${e}/${ID}/rsvp`], ["GET", `${e}/${ID}/invitations`], ["POST", `${e}/media/${ID}/parts/64`],
    ["GET", `${e}/../policy`], ["GET", `${e}/${ID}/%2e%2e/x`], ["POST", `${e}/${ID}/cancel?reason=x`],
  ];
  for (const [method, path] of refused) assert.equal(isMessagingRelayRoute(method, path), false, `${method} ${path}`);
});

test("the service calls the relay with exact paths and an idempotency key for creation", () => {
  const src = readFileSync(new URL("../src/services/v2/events.service.ts", import.meta.url), "utf8");
  assert.match(src, /create: \(body: EventCreateRequest, idempotencyKey: string = crypto\.randomUUID\(\)\) => call<CalendarEvent>\("POST", E, body, idempotencyKey\)/);
  assert.match(src, /const E = "v1\/messaging\/events";/);
  for (const path of ["/cancel`", "/rsvp`", "/attendees`", "/invitations`", "/reminders`", "/ics`", "/calendar`", "/download-grants`", "/complete`"]) assert.ok(src.includes(path), path);
});

test("the bell opens an event notice on the event page", async () => {
  const { inboxHref } = await import("../src/lib/notifications/inbox.ts");
  assert.equal(inboxHref({ id: "1", action: { kind: "OPEN_EVENT", ref: ID } }), `/dashboard/events/${ID}`);
  assert.equal(inboxHref({ id: "2", action: { kind: "OPEN_EVENT", ref: null } }), "/dashboard/events");
});

test("community events (ADR 0071 / 0073): the form sends communityId, and the community page lists its own events", () => {
  const CM = "44444444-4444-4444-8444-444444444444";
  const state = { ...core.emptyForm("Europe/London", NOW, null, CM), title: "Street party", startDate: "2026-07-10", endDate: "2026-07-10" };
  assert.equal(state.scope, "COMMUNITY");
  const body = core.buildCreateBody(state);
  assert.equal(body.scope, "COMMUNITY");
  assert.equal(body.communityId, CM);
  assert.equal(body.conversationId, undefined);
  assert.equal(core.validateForm({ ...state, communityId: "" }, NOW), "Choose the community this event belongs to.");
  const item = (scope, communityId, eventId) => ({ startsAt: "2026-07-10T18:00:00Z", endsAt: "2026-07-10T19:00:00Z", event: { eventId, scope, communityId } });
  const items = [item("COMMUNITY", CM, "a"), item("COMMUNITY", C, "b"), item("CONVERSATION", null, "c"), item("PERSONAL", null, "d")];
  assert.deepEqual(core.communityEventItems(items, CM).map((i) => i.event.eventId), ["a"]);
});
