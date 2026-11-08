// Meetings + calendar (Blueprint Section 10 / 17): timezone conversion with the backend's DST
// rules, calendar ranges, opaque-cursor agenda pagination, occurrence controls, artefacts.
// Run: node --experimental-strip-types --test tests/meetings-calendar.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

function load(path, dependencies = {}) {
  const exports = {};
  const source = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  vm.runInNewContext(source, { exports, Date, Intl, Map, Set, Error, Promise, require: (name) => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency: ${name}`);
  } });
  return exports;
}

const time = load("../src/lib/meeting-time.ts");
const calendar = load("../src/lib/meeting-calendar.ts", { "./meeting-time": time });
const artifacts = load("../src/lib/artifact-display.ts");
const plain = (value) => JSON.parse(JSON.stringify(value));

const NY = "America/New_York", ROME = "Europe/Rome", LAGOS = "Africa/Lagos";

test("wall time in the meeting zone converts to UTC (no DST change)", () => {
  assert.equal(time.zonedLocalToUtc("2026-07-01T09:00", NY), "2026-07-01T13:00:00.000Z");
  assert.equal(time.zonedLocalToUtc("2026-01-15T09:00", NY), "2026-01-15T14:00:00.000Z");
  assert.equal(time.zonedLocalToUtc("2026-07-01T09:00", ROME), "2026-07-01T07:00:00.000Z");
  assert.equal(time.zonedLocalToUtc("2026-01-15T09:00", ROME), "2026-01-15T08:00:00.000Z");
  assert.equal(time.zonedLocalToUtc("2026-07-01T09:00", LAGOS), "2026-07-01T08:00:00.000Z");
  assert.equal(time.zonedLocalToUtc("2026-01-15T09:00", LAGOS), "2026-01-15T08:00:00.000Z");
  assert.equal(time.zonedLocalToUtc("2026-01-15T09:00", "UTC"), "2026-01-15T09:00:00.000Z");
  // Independent of the browser zone: the round trip is exact.
  for (const zone of [NY, ROME, LAGOS]) assert.equal(time.utcToZonedLocal(time.zonedLocalToUtc("2026-05-20T17:45", zone), zone), "2026-05-20T17:45");
});

test("DST gap: a nonexistent local time occurs at the shifted instant (02:30 -> 03:30)", () => {
  // New York springs forward 2026-03-08 02:00 EST -> 03:00 EDT.
  const ny = time.zonedLocalToUtc("2026-03-08T02:30", NY);
  assert.equal(ny, "2026-03-08T07:30:00.000Z");
  assert.equal(time.utcToZonedLocal(ny, NY), "2026-03-08T03:30");
  assert.equal(time.localTimeKind("2026-03-08T02:30", NY), "nonexistent");
  // Rome springs forward 2026-03-29 02:00 CET -> 03:00 CEST.
  const rome = time.zonedLocalToUtc("2026-03-29T02:30", ROME);
  assert.equal(rome, "2026-03-29T01:30:00.000Z");
  assert.equal(time.utcToZonedLocal(rome, ROME), "2026-03-29T03:30");
  assert.equal(time.localTimeKind("2026-03-29T02:30", ROME), "nonexistent");
  // Lagos has no DST: the same wall time exists.
  assert.equal(time.zonedLocalToUtc("2026-03-29T02:30", LAGOS), "2026-03-29T01:30:00.000Z");
  assert.equal(time.localTimeKind("2026-03-29T02:30", LAGOS), "ok");
});

test("DST overlap: a repeated local time takes the first instant", () => {
  // New York falls back 2026-11-01 02:00 EDT -> 01:00 EST; 01:30 happens twice.
  assert.equal(time.zonedLocalToUtc("2026-11-01T01:30", NY), "2026-11-01T05:30:00.000Z");
  assert.equal(time.localTimeKind("2026-11-01T01:30", NY), "ambiguous");
  assert.equal(time.localTimeKind("2026-11-01T03:30", NY), "ok");
  // Rome falls back 2026-10-25 03:00 CEST -> 02:00 CET; 02:30 happens twice.
  assert.equal(time.zonedLocalToUtc("2026-10-25T02:30", ROME), "2026-10-25T00:30:00.000Z");
  assert.equal(time.localTimeKind("2026-10-25T02:30", ROME), "ambiguous");
  assert.equal(time.localTimeKind("2026-10-25T02:30", LAGOS), "ok");
});

test("meeting-zone and viewer-zone labels; same offset shows one label", () => {
  const at = "2026-11-09T14:00:00.000Z";
  const label = time.dualZoneLabel(at, LAGOS, NY);
  assert.equal(label.zone, LAGOS);
  assert.match(label.primary, /3:00 PM/);
  assert.match(label.secondary, /9:00 AM.*\(your time\)/);
  assert.equal(time.dualZoneLabel(at, LAGOS, LAGOS).secondary, null);
  // Rome and Lagos share UTC+1 in winter: no redundant viewer label.
  assert.equal(time.dualZoneLabel(at, LAGOS, ROME).secondary, null);
  // ...but not in summer.
  assert.notEqual(time.dualZoneLabel("2026-07-09T14:00:00.000Z", LAGOS, ROME).secondary, null);
  // An invalid meeting zone falls back to the viewer zone.
  assert.equal(time.dualZoneLabel(at, "Not/AZone", NY).zone, NY);
  const window = time.scheduleWindowLabel(at, "2026-11-09T15:00:00.000Z", NY, NY);
  assert.match(window.primary, /9:00 AM.*– 10:00 AM/);
});

test("lists show the scheduled start of latestConference, never createdAt (MTG-004)", () => {
  const now = Date.parse("2026-11-08T12:00:00Z");
  const upcoming = time.scheduleSummary({ id: "c1", status: "CREATED", scheduledStartAt: "2026-11-10T09:00:00Z" }, ROME, now, NY);
  assert.equal(upcoming.state, "UPCOMING");
  assert.match(upcoming.primary, /Nov 10, 2026, 10:00 AM/);
  assert.match(upcoming.secondary, /4:00 AM/);
  assert.equal(time.scheduleSummary({ id: "c1", status: "LIVE", startedAt: "2026-11-08T11:00:00Z" }, ROME, now, NY).state, "LIVE");
  assert.equal(time.scheduleSummary({ id: "c1", status: "CREATED" }, ROME, now, NY).state, "UNSCHEDULED");
  assert.equal(time.scheduleSummary({ id: "c1", status: "CANCELLED", scheduledStartAt: "2026-11-01T09:00:00Z" }, ROME, now, NY).label, "Cancelled");
  assert.equal(time.scheduleSummary(null, ROME, now, NY).state, "NONE");
  // Healthcare / high-security summaries carry no timing: status only.
  assert.equal(time.scheduleSummary({ id: "c1", status: "CREATED" }, undefined, now, NY).primary, null);
});

test("occurrence windows are typed in the meeting zone and follow the backend rules", () => {
  const now = Date.parse("2026-11-08T12:00:00Z");
  assert.deepEqual(plain(time.occurrenceWindowInZone("2026-11-10T09:00", "2026-11-10T10:30", ROME, now)), { scheduledStartAt: "2026-11-10T08:00:00.000Z", scheduledEndAt: "2026-11-10T09:30:00.000Z" });
  assert.deepEqual(plain(time.occurrenceWindowInZone("2026-11-10T09:00", "", NY, now)), { scheduledStartAt: "2026-11-10T14:00:00.000Z" });
  assert.throws(() => time.occurrenceWindowInZone("2026-11-08T06:00", "", NY, now), /future/);
  assert.throws(() => time.occurrenceWindowInZone("2026-11-10T09:00", "2026-11-10T09:00", LAGOS, now), /after the start/);
  assert.throws(() => time.occurrenceWindowInZone("2026-11-10T09:00", "2026-11-11T09:01", LAGOS, now), /24 hours/);
  assert.throws(() => time.occurrenceWindowInZone("tomorrow", "", LAGOS, now));
  assert.throws(() => time.zonedLocalToUtc("2026-11-10T09:00", "Mars/Olympus"));
  // A spring-forward start moves to the shifted instant, the end stays exact.
  assert.deepEqual(plain(time.occurrenceWindowInZone("2027-03-14T02:30", "2027-03-14T04:00", NY, now)), { scheduledStartAt: "2027-03-14T07:30:00.000Z", scheduledEndAt: "2027-03-14T08:00:00.000Z" });
});

test("date-range filters use an exclusive next-midnight bound in the meeting zone (MTG-021)", () => {
  assert.deepEqual(plain(time.zonedDateRange("2026-11-01", "2026-11-01", NY)), { from: "2026-11-01T04:00:00.000Z", to: "2026-11-02T05:00:00.000Z" });
  assert.deepEqual(plain(time.zonedDateRange("2026-03-29", "2026-03-29", ROME)), { from: "2026-03-28T23:00:00.000Z", to: "2026-03-29T22:00:00.000Z" });
});

test("calendar views: day, week (DST week), month grid and agenda ranges in the viewer zone", () => {
  assert.deepEqual(plain(calendar.viewRange("day", "2026-03-08", NY)), { start: "2026-03-08", end: "2026-03-09", from: "2026-03-08T05:00:00.000Z", to: "2026-03-09T04:00:00.000Z" });
  const week = calendar.viewRange("week", "2026-03-11", NY);
  assert.deepEqual(plain(week), { start: "2026-03-09", end: "2026-03-16", from: "2026-03-09T04:00:00.000Z", to: "2026-03-16T04:00:00.000Z" });
  assert.equal(calendar.viewRange("week", "2026-03-08", NY).from, "2026-03-02T05:00:00.000Z");
  assert.equal(calendar.viewRange("week", "2026-03-08", NY).to, "2026-03-09T04:00:00.000Z");
  // November 2026 starts on a Sunday: the Monday-first grid starts on 26 October.
  const month = calendar.viewDates("month", "2026-11-17");
  assert.deepEqual(plain(month), { start: "2026-10-26", end: "2026-12-07" });
  assert.equal(calendar.datesBetween(month.start, month.end).length, 42);
  assert.deepEqual(plain(calendar.viewDates("month", "2026-02-10")), { start: "2026-01-26", end: "2026-03-02" });
  assert.equal(calendar.viewDates("week", "2026-11-08", 0).start, "2026-11-08");
  assert.deepEqual(plain(calendar.viewDates("agenda", "2026-11-08")), { start: "2026-11-08", end: "2026-12-09" });
  assert.equal(calendar.viewRange("month", "2026-10-10", ROME).from, "2026-09-27T22:00:00.000Z");
  assert.equal(calendar.viewRange("day", "2026-10-25", ROME).to, "2026-10-25T23:00:00.000Z");
  assert.equal(calendar.shiftAnchor("month", "2026-01-31", 1), "2026-02-28");
  assert.equal(calendar.shiftAnchor("week", "2026-11-08", -1), "2026-11-01");
  assert.equal(calendar.todayIn(LAGOS, Date.parse("2026-11-08T23:30:00Z")), "2026-11-09");
  assert.equal(calendar.todayIn(NY, Date.parse("2026-11-08T23:30:00Z")), "2026-11-08");
});

const item = (overrides = {}) => ({
  conferenceId: "11111111-1111-4111-8111-111111111111",
  meetingSpaceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  occurrenceKey: "single:1",
  kind: "SCHEDULED",
  status: "CREATED",
  view: "FULL",
  scheduledStartAt: "2026-11-10T09:00:00Z",
  scheduledEndAt: "2026-11-10T10:00:00Z",
  isException: false,
  materialized: true,
  relations: ["OWNER"],
  viewerRole: "OWNER",
  meetingSpace: { meetingCode: "abc-defg-hij", meetingType: "STANDARD", accessPolicy: "PUBLIC", requiresPasscode: false, status: "ACTIVE", timezone: ROME, title: "Sync" },
  ...overrides,
});

test("agenda pagination follows the opaque cursor with the same filters, dedupes and bounds", async () => {
  const pages = {
    "": { items: [item({ occurrenceKey: "rrule:r:2026-11-10T09:00:00Z", conferenceId: null, materialized: false })], nextCursor: "c/1==" },
    "c/1==": { items: [item({ occurrenceKey: "single:2", scheduledStartAt: "2026-11-11T09:00:00Z" })], nextCursor: "c/2==" },
    "c/2==": { items: [item({ occurrenceKey: "rrule:r:2026-11-10T09:00:00Z" }), item({ occurrenceKey: "single:0", scheduledStartAt: "2026-11-09T09:00:00Z" })], nextCursor: null },
  };
  const calls = [];
  const fetchPage = async (query) => { calls.push(query); return pages[query.cursor ?? ""]; };
  const query = { from: "2026-11-09T00:00:00Z", to: "2026-11-16T00:00:00Z", organizationId: "org-1", status: "CREATED,LIVE" };
  const result = await calendar.fetchAgendaRange(fetchPage, query);
  assert.equal(result.pages, 3);
  assert.equal(result.truncated, false);
  assert.deepEqual(calls.map((c) => c.cursor), [undefined, "c/1==", "c/2=="]);
  for (const call of calls) {
    assert.equal(call.organizationId, "org-1");
    assert.equal(call.from, query.from);
    assert.equal(call.limit, calendar.AGENDA_PAGE_LIMIT);
  }
  // The materialized copy of the same occurrence key replaced the virtual one.
  assert.deepEqual(plain(result.items.map((i) => i.occurrenceKey)), ["single:0", "rrule:r:2026-11-10T09:00:00Z", "single:2"]);
  assert.equal(result.items[1].materialized, true);

  const capped = await calendar.fetchAgendaRange(fetchPage, query, { maxPages: 2 });
  assert.equal(capped.truncated, true);
  assert.equal(capped.pages, 2);

  let loops = 0;
  const looping = await calendar.fetchAgendaRange(async () => { loops += 1; return { items: [], nextCursor: "same" }; }, query);
  assert.equal(loops, 2);
  assert.equal(looping.truncated, false);

  assert.deepEqual(plain(calendar.agendaParams({ from: "x", organizationId: "", status: undefined, role: null, limit: 50 })), { from: "x", limit: 50 });
});

test("agenda entries group by the viewer's local date and place in the day grid", () => {
  const groups = calendar.groupByLocalDate([item({ scheduledStartAt: "2026-11-10T03:30:00Z" }), item({ occurrenceKey: "b", scheduledStartAt: "2026-11-10T09:00:00Z" })], NY);
  assert.deepEqual([...groups.keys()], ["2026-11-09", "2026-11-10"]);
  assert.deepEqual(plain(calendar.dayPlacement(item({ scheduledEndAt: null }), ROME)), { date: "2026-11-10", startMinute: 600, durationMinutes: 60 });
  assert.equal(calendar.dayPlacement(item({ scheduledStartAt: "2026-11-10T22:30:00Z", scheduledEndAt: "2026-11-11T01:00:00Z" }), "UTC").durationMinutes, 90);
});

test("occurrence controls follow role, view, status and materialization", () => {
  assert.deepEqual(plain(calendar.occurrenceActions(item())), { join: true, reschedule: true, cancel: true, exception: false, recurring: false, ics: true, originalStartAt: null });
  const member = calendar.occurrenceActions(item({ view: "SUMMARY", viewerRole: "PARTICIPANT" }));
  assert.equal(member.join, true);
  assert.equal(member.reschedule, false);
  assert.equal(member.cancel, false);
  assert.equal(calendar.occurrenceActions(item({ viewerRole: "CO_HOST" })).reschedule, false);
  assert.equal(calendar.occurrenceActions(item({ status: "WAITING" })).reschedule, false);
  assert.equal(calendar.occurrenceActions(item({ status: "WAITING" })).cancel, true);
  assert.equal(calendar.occurrenceActions(item({ status: "ENDED" })).join, false);
  assert.equal(calendar.occurrenceActions(item({ meetingSpace: { ...item().meetingSpace, status: "ARCHIVED" } })).join, false);
  const virtual = calendar.occurrenceActions(item({ conferenceId: null, materialized: false, occurrenceKey: "rrule:r1:2027-01-04T08:00:00Z", scheduledStartAt: "2027-01-04T08:00:00Z" }));
  assert.deepEqual(plain(virtual), { join: false, reschedule: false, cancel: false, exception: true, recurring: true, ics: false, originalStartAt: "2027-01-04T08:00:00Z" });
  const movedVirtual = calendar.occurrenceActions(item({ conferenceId: null, materialized: false, occurrenceKey: "rrule:r1:x", isException: true, originalStartAt: null }));
  assert.equal(movedVirtual.exception, false);
  assert.equal(calendar.occurrenceActions(item({ conferenceId: null, materialized: false, occurrenceKey: "rrule:r1:x", view: "SUMMARY", viewerRole: "MODERATOR" })).exception, false);
});

test("artefact views read only fields the backend sends (FE-007)", () => {
  const view = {
    id: "art-1", conferenceId: "c1", artifactType: "RECORDING", status: "READY", contentType: "video/mp4", sizeBytes: 5_242_880, durationMs: 3_723_000,
    metadata: {}, createdAt: "2026-11-08T10:00:00Z", updatedAt: "2026-11-08T11:00:00Z", finalizedAt: "2026-11-08T11:00:00Z", deletionPending: false,
  };
  assert.equal(artifacts.artifactDisplayName(view, "UTC"), "Recording · 8 Nov 2026, 10:00");
  assert.equal(artifacts.formatBytes(view.sizeBytes), "5 MB");
  assert.equal(artifacts.formatBytes(1536), "1.5 KB");
  assert.equal(artifacts.formatBytes(null), null);
  assert.equal(artifacts.formatDurationMs(view.durationMs), "1:02:03");
  assert.equal(artifacts.formatDurationMs(65_000), "1:05");
  assert.equal(artifacts.artifactAccessible(view), true);
  assert.equal(artifacts.artifactAccessible({ ...view, deletionPending: true }), false);
  assert.equal(artifacts.artifactStateLabel({ ...view, deletionPending: true }), "Deletion pending");
  assert.equal(artifacts.artifactStateLabel({ ...view, status: "FAILED", failureCode: "PROVIDER_ERROR" }), "Failed (PROVIDER_ERROR)");
  const rows = Object.fromEntries(artifacts.artifactDetailRows(view, "UTC"));
  assert.equal(rows["Content type"], "video/mp4");
  assert.equal(rows.Duration, "1:02:03");
  assert.ok(rows.Finalized);
  assert.equal(rows.Layout, undefined);
  assert.equal(artifacts.artifactTypeLabel("WHITEBOARD_SNAPSHOT"), "Whiteboard snapshot");
});
