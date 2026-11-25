// Chat controls (server audit F1, F4-F7):
// F1  archive / pin a chat for yourself: archived chats leave the main list, pinned chats come
//     first (most recently pinned first), at most three pins.
// F4-F7 polls, locations, contact cards and link previews travel inside a private chat's
//     encrypted content; a link preview is built from the link itself (nothing is fetched)
//     and the crypto worker accepts only these bounded shapes.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { MAX_PINNED_CHATS, arrangeChats, inboxFlags, pinnedCount } from "../src/lib/messaging/chat-list.ts";
import { linkPreviewFor, richContent, richSummary, tallyVotes, privateReactionState } from "../src/lib/messaging/rich-content.ts";

const chat = (id, at, flags = {}) => ({ id, lastActivityAt: at, archived: false, pinned: false, pinnedAt: null, ...flags });

test("archived chats leave the main list and pinned chats come first", () => {
  const list = [
    chat("a", "2026-11-20T10:00:00Z"),
    chat("b", "2026-11-20T09:00:00Z", { pinned: true, pinnedAt: "2026-11-19T08:00:00Z" }),
    chat("c", "2026-11-20T08:00:00Z", { archived: true }),
    chat("d", "2026-11-20T07:00:00Z", { pinned: true, pinnedAt: "2026-11-19T09:00:00Z" }),
  ];
  assert.deepEqual(arrangeChats(list, false).map((c) => c.id), ["d", "b", "a"]);
  assert.deepEqual(arrangeChats(list, true).map((c) => c.id), ["c"]);
  assert.equal(pinnedCount(list), 2);
  assert.equal(MAX_PINNED_CHATS, 3);
});

test("inbox flags come from the server summary, kept when an older server omits them", () => {
  assert.deepEqual(inboxFlags({ archived: true, pinned: false, pinnedAt: null }), { archived: true, pinned: false, pinnedAt: null });
  const previous = { archived: false, pinned: true, pinnedAt: "2026-11-19T08:00:00Z" };
  assert.deepEqual(inboxFlags({ unreadCount: 1 }, previous), previous);
  assert.deepEqual(inboxFlags(undefined), { archived: false, pinned: false, pinnedAt: null });
});

test("a link preview is the link and its site, built without fetching anything", () => {
  assert.deepEqual(linkPreviewFor("see https://www.example.com/a?b=1, thanks"), { url: "https://www.example.com/a?b=1", title: null, description: null, siteName: "example.com" });
  assert.equal(linkPreviewFor("no link here"), null);
  assert.equal(linkPreviewFor("javascript:alert(1)"), null);
});

test("poll votes: the latest vote of each person counts, an empty vote retracts", () => {
  const poll = "11111111-1111-4111-8111-111111111111";
  const votes = [
    { poll, choices: [0], sender: "p1", sequence: 1 },
    { poll, choices: [1], sender: "p1", sequence: 3 },
    { poll, choices: [1], sender: "p2", sequence: 2 },
    { poll, choices: [], sender: "p3", sequence: 4 },
    { poll: "other", choices: [0], sender: "p4", sequence: 5 },
  ];
  assert.deepEqual(tallyVotes(votes, poll, 2, "p1"), { counts: [0, 2], voters: 2, myChoices: [1] });
  const rich = richContent({ t: "poll", question: "Lunch?", options: ["Yes", "No"], multiple: false }, poll, votes, "p2");
  assert.equal(rich.kind, "poll");
  assert.deepEqual(rich.tally.myChoices, [1]);
  assert.equal(richSummary({ t: "poll", question: "Lunch?" }), "Poll: Lunch?");
  assert.equal(richSummary({ t: "contact", name: "Ada", cfMeetId: "ada.l" }), "Contact: Ada");
  assert.equal(richSummary({ t: "location", label: null }), "Location");
  assert.equal(richContent({ t: "text", text: "hi" }, null, [], null), null);
});

test("the crypto worker allows only the bounded payload shapes", () => {
  const src = readFileSync(new URL("../messaging-crypto/worker.src.js", import.meta.url), "utf8");
  for (const kind of ['"contact"', '"location"', '"poll"', '"vote"']) assert.ok(src.includes(`case ${kind}:`), kind);
  // Anything else is refused on the way in and out.
  assert.match(src, /default:\s*return fail\("PAYLOAD_INVALID"\);/);
  // Only http(s) links are previewed.
  assert.match(src, /parsed\.protocol !== "https:" && parsed\.protocol !== "http:"/);
});


test("private reaction state uses canonical control order, deduplicates devices per principal, and removes without resurrecting older adds", () => {
  const targets = new Map([["message", 1]]);
  const controls = [
    { target: "message", reaction: ":yes:", add: false, sender: "A", sequence: 5 },
    { target: "message", reaction: ":yes:", add: true, sender: "A", sequence: 2 },
    { target: "message", reaction: ":yes:", add: true, sender: "A", sequence: 4 },
    { target: "message", reaction: ":yes:", add: true, sender: "B", sequence: 3 },
    { target: "message", reaction: ":yes:", add: true, sender: "B", sequence: 3 },
    { target: "other-conversation", reaction: ":yes:", add: true, sender: "C", sequence: 8 },
  ];
  assert.deepEqual(privateReactionState(controls, targets, "A").get("message"), [{ reaction: ":yes:", count: 1, reactedByMe: false }]);
  assert.deepEqual(privateReactionState(controls, targets, "B").get("message"), [{ reaction: ":yes:", count: 1, reactedByMe: true }]);
  assert.equal(privateReactionState(controls, new Map(), "B").size, 0, "deleted or unloaded targets cannot acquire badges");
  assert.equal(privateReactionState(controls, new Map([["message", 20]]), "B").size, 0, "controls cannot target later messages");
});

test("private reaction rendering bounds per-person badges while retaining removals", () => {
  const controls = Array.from({ length: 25 }, (_, index) => ({ target: "message", reaction: `:r${index}:`, add: true, sender: "A", sequence: index + 2 }));
  assert.equal(privateReactionState(controls, new Map([["message", 1]]), "A").get("message").length, 20);
});
