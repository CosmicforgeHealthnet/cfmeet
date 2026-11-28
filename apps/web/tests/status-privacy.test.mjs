// Status privacy and management (ADR 0069): the pure rules behind the audience button and
// picker, the Status privacy screen, @mentions, My status, rings and status search.
// Run: node --experimental-strip-types --test tests/status-privacy.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
  audienceButtonLabel, audienceMembers, audienceShortLabel, canShare, deselectAll, insertMention, matchContacts, mentionQuery,
  mentionsInText, mentionsWithinAudience, normalizeSelection, reactionSummary, ringArcs, ringBatches, ringLook, searchPeople,
  searchStatusAuthors, selectAll, selectionToChoice, snapshotLabel, timeLeft, toggle, uniqueViewers,
} from "../src/lib/status-privacy.ts";
import { StatusQueue } from "../src/lib/status-publisher/queue.ts";
import { MemoryQueueStorage } from "../src/lib/status-publisher/storage.ts";

const lists = [{ listId: "L1", name: "Family", memberPrincipalIds: ["a", "b", "z"] }];

test("the audience button reads like WhatsApp", () => {
  assert.equal(audienceButtonLabel(null), "Status (My contacts)");
  assert.equal(audienceButtonLabel({ mode: "CONTACTS" }), "Status (My contacts)");
  assert.equal(audienceButtonLabel({ mode: "CONTACTS_EXCEPT", principalIds: Array.from({ length: 8 }, (_, i) => `p${i}`) }), "Status (8 excluded)");
  assert.equal(audienceButtonLabel({ mode: "CONTACTS_EXCEPT", principalIds: [] }), "Status (My contacts)");
  assert.equal(audienceButtonLabel({ mode: "ONLY", principalIds: Array.from({ length: 12 }, (_, i) => `p${i}`) }), "Status (12 selected)");
  assert.equal(audienceButtonLabel({ mode: "CLOSE_FRIENDS" }), "Close friends");
  assert.equal(audienceButtonLabel({ mode: "CUSTOM", listId: "L1" }, lists), "Family");
  assert.equal(audienceShortLabel({ mode: "CUSTOM", listId: "gone" }, lists), "Custom list");
});

test("a published status's snapshot label comes from its stored counts", () => {
  assert.equal(snapshotLabel({ mode: "CONTACTS_EXCEPT", label: null, includedCount: 10, excludedCount: 3 }), "3 excluded");
  assert.equal(snapshotLabel({ mode: "ONLY", label: null, includedCount: 2, excludedCount: 0 }), "2 selected");
  assert.equal(snapshotLabel({ mode: "CUSTOM", label: "Work", includedCount: 4, excludedCount: 0 }), "Work");
  assert.equal(snapshotLabel(undefined), "My contacts");
});

test("choices are normalised to what the server accepts", () => {
  const eligible = ["a", "b", "c"];
  assert.deepEqual(normalizeSelection({ mode: "ONLY", principalIds: ["a", "x", "a"] }, eligible), { mode: "ONLY", principalIds: ["a"] });
  assert.equal(normalizeSelection({ mode: "ONLY", principalIds: ["x"] }, eligible), null, "ONLY needs someone eligible");
  assert.equal(normalizeSelection({ mode: "CUSTOM", listId: "nope" }, eligible, lists), null);
  assert.deepEqual(normalizeSelection({ mode: "CUSTOM", listId: "L1" }, eligible, lists), { mode: "CUSTOM", listId: "L1" });
  assert.deepEqual(normalizeSelection({ mode: "CLOSE_FRIENDS", principalIds: ["a"] }, eligible), { mode: "CLOSE_FRIENDS" });
  assert.deepEqual(selectionToChoice({ mode: "CUSTOM", listId: "L1", principalIds: ["a"] }), { mode: "CUSTOM", listId: "L1" });
  assert.deepEqual(selectionToChoice({ mode: "CONTACTS_EXCEPT", principalIds: ["a"] }), { mode: "CONTACTS_EXCEPT", principalIds: ["a"] });
});

test("who a choice reaches: only eligible contacts, whatever the lists say", () => {
  const eligible = ["a", "b", "c"];
  assert.deepEqual(audienceMembers({ mode: "CONTACTS" }, eligible), ["a", "b", "c"]);
  assert.deepEqual(audienceMembers({ mode: "CONTACTS_EXCEPT", principalIds: ["b"] }, eligible), ["a", "c"]);
  assert.deepEqual(audienceMembers({ mode: "ONLY", principalIds: ["c", "zz"] }, eligible), ["c"]);
  assert.deepEqual(audienceMembers({ mode: "CLOSE_FRIENDS" }, eligible, ["b", "stranger"]), ["b"]);
  assert.deepEqual(audienceMembers({ mode: "CUSTOM", listId: "L1" }, eligible, [], lists), ["a", "b"]);
});

test("people search, select all and deselect all act on the shown people only", () => {
  const people = [{ name: "Zoë Adams" }, { name: "Bob" }, { name: "zoe b" }];
  assert.deepEqual(searchPeople(people, "ZOE").map((p) => p.name), ["Zoë Adams", "zoe b"]);
  assert.deepEqual(searchPeople(people, " "), people);
  assert.deepEqual(toggle(["a"], "b"), ["a", "b"]);
  assert.deepEqual(toggle(["a", "b"], "a"), ["b"]);
  assert.deepEqual(selectAll(["x"], ["a", "b", "x"]), ["x", "a", "b"]);
  assert.deepEqual(deselectAll(["x", "a", "b"], ["a", "b"]), ["x"]);
});

test("@mentions: the query, the inserted name, and only names still in the text within the audience", () => {
  assert.deepEqual(mentionQuery("hi @Bo", 6), { start: 3, query: "Bo" });
  assert.equal(mentionQuery("mail@bob", 8), null, "inside a word is not a mention");
  assert.equal(mentionQuery("hi @Bob\nx", 9), null);
  assert.deepEqual(mentionQuery("@", 1), { start: 0, query: "" });
  const ins = insertMention("hi @Bo there", 6, "Bob Smith");
  assert.equal(ins.text, "hi @Bob Smith  there");
  assert.equal(ins.caret, 14);
  const picked = [{ principalId: "b", name: "Bob Smith" }, { principalId: "c", name: "Cara" }];
  assert.deepEqual(mentionsInText("hi @Bob Smith", picked), ["b"], "Cara's name was deleted");
  assert.deepEqual(mentionsWithinAudience(["b", "c"], ["c"]), ["c"]);
});

test("time left, unique viewers and reactions", () => {
  const now = Date.parse("2026-11-28T10:00:00Z");
  assert.equal(timeLeft("2026-11-29T09:30:00Z", now), "23h left");
  assert.equal(timeLeft("2026-11-28T10:45:00Z", now), "45m left");
  assert.equal(timeLeft("2026-11-28T10:00:30Z", now), "Expiring");
  const viewers = uniqueViewers([
    { principalId: "a", viewedAt: "2026-11-28T09:00:00Z", reaction: null },
    { principalId: "b", viewedAt: "2026-11-28T09:30:00Z", reaction: "😂" },
    { principalId: "a", viewedAt: "2026-11-28T09:45:00Z", reaction: "🔥" },
  ]);
  assert.deepEqual(viewers.map((v) => [v.principalId, v.reaction]), [["a", "🔥"], ["b", "😂"]]);
  assert.deepEqual(reactionSummary([{ principalId: "a", viewedAt: "", liked: true }, { principalId: "b", viewedAt: "", reaction: "😂" }, { principalId: "c", viewedAt: "", reaction: "😂" }]),
    [{ emoji: "😂", count: 2 }, { emoji: "❤️", count: 1 }]);
});

test("rings: none when nothing is visible, segmented, unseen / seen, close friends", () => {
  assert.deepEqual(ringLook(null), { kind: "none" });
  assert.deepEqual(ringLook({ principalId: "a", hasActiveStatus: false, count: 0, unseenCount: 0, closeFriends: false }), { kind: "none" });
  assert.deepEqual(ringLook({ principalId: "a", hasActiveStatus: true, count: 3, unseenCount: 1, closeFriends: true }),
    { kind: "ring", unseen: true, closeFriends: true, segments: 3, seenSegments: 2 });
  assert.equal(ringLook({ principalId: "a", hasActiveStatus: true, count: 2, unseenCount: 0, closeFriends: false }).unseen, false);
  assert.deepEqual(ringArcs(1, 0), [{ from: 0, to: 360, seen: false }]);
  const arcs = ringArcs(4, 1);
  assert.equal(arcs.length, 4);
  assert.deepEqual(arcs.map((a) => a.seen), [true, false, false, false]);
  assert.ok(arcs.every((a, i) => a.to > a.from && (i === 0 || a.from > arcs[i - 1].to)), "gaps between segments");
  const batches = ringBatches([...Array.from({ length: 250 }, (_, i) => `p${String(i).padStart(3, "0")}`), "p000", null, undefined]);
  assert.deepEqual(batches.map((b) => b.length), [200, 50], "unique, at most 200 per request");
});

test("contacts match principals only when the name is unambiguous", () => {
  const contacts = [
    { cfMeetId: "CF-1", displayName: "Ada Lovelace", alias: null },
    { cfMeetId: "CF-2", displayName: "Sam", alias: null },
    { cfMeetId: "CF-3", displayName: "Sam", alias: null },
    { cfMeetId: "CF-4", displayName: "Bo", alias: "Bobby" },
  ];
  const principals = [
    { principalId: "pa", displayName: "ada lovelace" },
    { principalId: "ps", displayName: "Sam" },
    { principalId: "pb", displayName: "Bo", alias: "Bobby" },
    { principalId: "pb2", displayName: "Bo" },
  ];
  const m = matchContacts(contacts, principals);
  assert.equal(m.get("CF-1"), "pa");
  assert.equal(m.get("CF-2"), undefined, "two contacts named Sam: no ring rather than a wrong one");
  assert.equal(m.get("CF-4"), "pb");
});

test("status search and sharing", () => {
  const authors = [
    { person: { name: "Ada" }, posts: [{ text: "Hello", caption: null }] },
    { person: { name: "Bob" }, posts: [{ text: null, caption: "Beach day" }] },
  ];
  assert.deepEqual(searchStatusAuthors(authors, "beach").map((a) => a.person.name), ["Bob"]);
  assert.deepEqual(searchStatusAuthors(authors, "ad").map((a) => a.person.name), ["Ada"]);
  assert.equal(searchStatusAuthors(authors, "").length, 2);
  assert.equal(canShare({ allowSharing: false }, false), false);
  assert.equal(canShare({ allowSharing: true }, true), false, "never on your own status");
  assert.equal(canShare({}, false), true);
});

test("the queue sends the chosen audience with every split segment, mentions with the first", async () => {
  const creates = [];
  let n = 0;
  const transport = {
    capabilities: { listParts: true, idempotentCreate: true, cancelUpload: true },
    async startUpload(req) { return { uploadId: `u${++n}`, partSize: 1024, partCount: Math.max(1, Math.ceil(req.sizeBytes / 1024)) }; },
    async storedParts() { return []; },
    async uploadPart() {},
    async completeUpload(id) { return { mediaId: `m-${id}` }; },
    async cancelUpload() {},
    async createStatus(req) { creates.push(req); return { postId: `s${creates.length}` }; },
  };
  const q = new StatusQueue({ storage: new MemoryQueueStorage(), transport, persistent: false });
  await q.start("owner", true);
  const done = new Promise((resolve) => q.onEvent((e) => { if (e.type === "published") resolve(e.job); }));
  const audience = { mode: "CONTACTS_EXCEPT", principalIds: ["x", "y"] };
  await q.enqueue({
    items: [{ kind: "VIDEO", source: new Blob(["v".repeat(10)], { type: "video/mp4" }), durationMs: 150_000, ranges: [{ startMs: 0, endMs: 60_000 }, { startMs: 60_000, endMs: 120_000 }, { startMs: 120_000, endMs: 150_000 }], caption: "hi @Bo", mentions: ["bo", "bo"] }],
    audience, allowSharing: false, audienceLabel: "2 excluded",
  });
  assert.equal(q.getSnapshot().jobs[0].audienceLabel, "2 excluded");
  const job = await done;
  assert.equal(job.postCount, 3);
  assert.equal(creates.length, 3);
  for (const c of creates) {
    assert.deepEqual(c.audience, audience, "every segment carries the audience");
    assert.equal(c.allowSharing, false);
  }
  assert.deepEqual(creates[0].mentions, ["bo"]);
  assert.equal(creates[1].mentions, undefined);
  assert.equal(creates[0].caption, "hi @Bo");
  q.stop();
});

test("without a choice the queue sends no audience (the server applies the saved default)", async () => {
  const creates = [];
  const transport = {
    capabilities: { listParts: true, idempotentCreate: true, cancelUpload: true },
    async startUpload() { return { uploadId: "u", partSize: 1024, partCount: 1 }; },
    async storedParts() { return []; },
    async uploadPart() {},
    async completeUpload() { return { mediaId: "m" }; },
    async cancelUpload() {},
    async createStatus(req) { creates.push(req); return { postId: "s" }; },
  };
  const q = new StatusQueue({ storage: new MemoryQueueStorage(), transport, persistent: false });
  await q.start("owner", true);
  const done = new Promise((resolve) => q.onEvent((e) => { if (e.type === "published") resolve(e.job); }));
  await q.enqueue({ items: [{ kind: "TEXT", text: "hello" }] });
  const job = await done;
  assert.equal(job.audienceLabel, null);
  assert.equal("audience" in creates[0], false);
  assert.equal("allowSharing" in creates[0], false);
  q.stop();
});
