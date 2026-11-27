// Chat controls (server audit F1, F4-F7):
// F1  archive / pin a chat for yourself: archived chats leave the main list, pinned chats come
//     first (most recently pinned first), at most three pins.
// F4-F7 polls, locations, contact cards and link previews travel inside a private chat's
//     encrypted content; a link preview is built from the link itself (nothing is fetched)
//     and the crypto worker accepts only these bounded shapes.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import {
  ALL_CHATS, MAX_PINNED_CHATS, arrangeChats, draftPreview, filterChips, filterKey, inboxFlags, liveFilter, matchesChatFilter, moveList, pinnedCount,
  summaryPreviewKind, validListName,
} from "../src/lib/messaging/chat-list.ts";
import { linkPreviewFor, managedPoll, pollFingerprint, richContent, richSummary, tallyVotes } from "../src/lib/messaging/rich-content.ts";
import { ManagedConversationState } from "../src/lib/messaging/managed-store.ts";

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
  assert.deepEqual(inboxFlags({ archived: true, pinned: false, pinnedAt: null, favourite: true }), { archived: true, pinned: false, pinnedAt: null, favourite: true });
  const previous = { archived: false, pinned: true, pinnedAt: "2026-11-19T08:00:00Z", favourite: true };
  assert.deepEqual(inboxFlags({ unreadCount: 1 }, previous), previous);
  // A server that predates favourites keeps what this device knew.
  assert.deepEqual(inboxFlags({ archived: false, pinned: true, pinnedAt: previous.pinnedAt }, previous), previous);
  assert.deepEqual(inboxFlags(undefined), { archived: false, pinned: false, pinnedAt: null, favourite: false });
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

// Favourites, chat lists, filter chips, row previews and drafts (WhatsApp chat list).
const conv = (id, flags = {}) => ({ id, unreadCount: 0, favourite: false, contextClass: "DIRECT", securityProfile: "MANAGED", ...flags });
const lists = [
  { listId: "l2", name: "Work", position: 1, conversationIds: ["b"] },
  { listId: "l1", name: "Family", position: 0, conversationIds: ["a", "b"] },
];

test("each chip shows its own chats; the open chat is never unread; a deleted list falls back to All", () => {
  const chats = [conv("a", { unreadCount: 2 }), conv("b", { favourite: true, contextClass: "GROUP" }), conv("c", { securityProfile: "PRIVATE_E2EE", unreadCount: 1 })];
  const shown = (filter, selected = null) => chats.filter((c) => matchesChatFilter(c, filter, { selected, lists })).map((c) => c.id);
  assert.deepEqual(shown(ALL_CHATS), ["a", "b", "c"]);
  assert.deepEqual(shown({ kind: "unread" }), ["a", "c"]);
  assert.deepEqual(shown({ kind: "unread" }, "a"), ["c"]);
  assert.deepEqual(shown({ kind: "favourites" }), ["b"]);
  assert.deepEqual(shown({ kind: "groups" }), ["b"]);
  assert.deepEqual(shown({ kind: "private" }), ["c"]);
  assert.deepEqual(shown({ kind: "list", listId: "l1" }), ["a", "b"]);
  assert.deepEqual(shown({ kind: "list", listId: "gone" }), []);
  assert.deepEqual(liveFilter({ kind: "list", listId: "gone" }, lists), ALL_CHATS);
  assert.equal(filterKey(liveFilter({ kind: "list", listId: "l2" }, lists)), "list:l2");
});

test("chips: All, Unread with its count, Favourites, Groups, Private only with private chats, then lists in order", () => {
  const chips = filterChips({ unread: 3, hasPrivate: false, lists });
  assert.deepEqual(chips.map((c) => c.label), ["All", "Unread", "Favourites", "Groups", "Family", "Work"]);
  assert.equal(chips[1].count, 3);
  assert.equal(filterChips({ unread: 0, hasPrivate: true, lists: [] }).map((c) => c.key).join(), "all,unread,favourites,groups,private");
  assert.equal(filterChips({ unread: 0, hasPrivate: false, lists: [] })[1].count, undefined);
});

test("lists: names are trimmed and bounded; moving one keeps the others in order", () => {
  assert.equal(validListName("  Family "), "Family");
  assert.equal(validListName("   "), null);
  assert.equal(validListName("x".repeat(41)), null);
  assert.equal(validListName("a\u0007b"), null);
  const three = [...lists, { listId: "l3", name: "Gym", position: 2, conversationIds: [] }];
  assert.deepEqual(moveList(three, "l3", 0).map((l) => [l.listId, l.position]), [["l3", 0], ["l1", 1], ["l2", 2]]);
  assert.deepEqual(moveList(three, "l1", 9).map((l) => l.listId), ["l2", "l3", "l1"]);
});

test("a row previews the last message's kind and an unsent draft", () => {
  const base = { attachmentCount: 0, previewText: "Lunch?", lifecycle: "ACTIVE" };
  assert.equal(summaryPreviewKind({ ...base, presentation: "POLL" }), "poll");
  assert.equal(summaryPreviewKind({ ...base, attachmentCount: 1, attachmentKind: "VOICE_NOTE" }), "voice");
  assert.equal(summaryPreviewKind({ ...base, attachmentCount: 1, attachmentKind: "IMAGE" }), "photo");
  assert.equal(summaryPreviewKind({ ...base, attachmentCount: 1, attachmentKind: "VIDEO" }), "video");
  assert.equal(summaryPreviewKind({ ...base, attachmentCount: 1, attachmentKind: "DOCUMENT" }), "document");
  // Older servers: the file name says the kind, a caption doesn't (a paperclip).
  assert.equal(summaryPreviewKind({ ...base, attachmentCount: 1, previewText: "report.pdf" }), "document");
  assert.equal(summaryPreviewKind({ ...base, attachmentCount: 1, previewText: "look at this" }), "file");
  assert.equal(summaryPreviewKind({ ...base, presentation: "POLL", lifecycle: "DELETED" }), null);
  assert.equal(summaryPreviewKind(base), null);
  assert.equal(draftPreview("  see you\n  tomorrow "), "see you tomorrow");
  assert.equal(draftPreview("   "), null);
  assert.equal(draftPreview("x".repeat(100)).length, 81);
});

test("a synced chat's poll shows its tally, my choices and who voted for what", () => {
  const poll = {
    options: [{ index: 1, text: "Sushi", voteCount: 1, voterPrincipalIds: ["p2"] }, { index: 0, text: "Pizza", voteCount: 2, voterPrincipalIds: ["p1", "p3"] }],
    allowsMultipleAnswers: false, voterCount: 3, closed: false, closedAt: null, closedByPrincipalId: null,
  };
  const rich = managedPoll("Lunch?", poll, "p1");
  assert.deepEqual(rich.options, ["Pizza", "Sushi"]);
  assert.deepEqual(rich.tally, { counts: [2, 1], voters: 3, myChoices: [0] });
  assert.deepEqual(rich.voters, [["p1", "p3"], ["p2"]]);
  assert.equal(rich.managed, true);
  assert.notEqual(pollFingerprint(poll), pollFingerprint({ ...poll, closed: true }));
});

test("poll events replace the poll message's tally (live results)", () => {
  const message = (voters, sequence = 4) => ({
    messageId: "m1", conversationId: "c1", sequence, kind: "APPLICATION", securityProfile: "MANAGED", senderPrincipalId: "p1",
    clientMessageId: "k1", lifecycle: "ACTIVE", editCount: 0, createdAt: "2026-11-27T10:00:00Z", attachmentIds: [],
    managed: { text: "Lunch?", presentation: "POLL", poll: { options: [{ index: 0, text: "Pizza", voteCount: voters.length, voterPrincipalIds: voters }, { index: 1, text: "Sushi", voteCount: 0, voterPrincipalIds: [] }], allowsMultipleAnswers: false, voterCount: voters.length, closed: false } },
  });
  const st = new ManagedConversationState();
  st.applyMessages([message([])]);
  assert.equal(st.get("m1").poll.voterCount, 0);
  const refresh = st.applyEvents([{ type: "poll.voted", conversationId: "c1", eventSequence: 9, data: { message: message(["p2"]) } }]);
  assert.ok(refresh.messages.has("m1"));
  assert.deepEqual(st.get("m1").poll.options[0].voterPrincipalIds, ["p2"]);
  // A deleted poll keeps no tally.
  st.applyMessages([{ ...message(["p2"]), lifecycle: "DELETED", managed: { text: null, presentation: "POLL", poll: null } }]);
  assert.equal(st.get("m1").poll, null);
});
