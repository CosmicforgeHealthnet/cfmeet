// MSG-005 / MSG-008: the MANAGED store converges on the server's current representation
// (edits, tombstones), never resurrects a deleted message, applies events once and in order,
// and merges Hub timeline cards by anchorSequence / occurredAt / entryId.
import assert from "node:assert/strict";
import test from "node:test";
import { ManagedConversationState, mergeTimeline, emptyRefresh } from "../src/lib/messaging/managed-store.ts";

const conv = "11111111-1111-4111-8111-111111111111";
const alice = "22222222-2222-4222-8222-222222222222";
const bob = "33333333-3333-4333-8333-333333333333";
let n = 0;
const id = () => `aaaaaaaa-aaaa-4aaa-8aaa-${String(++n).padStart(12, "0")}`;
function msg(sequence, extra = {}) {
  return {
    messageId: extra.messageId ?? id(), conversationId: conv, sequence, kind: "APPLICATION", securityProfile: "MANAGED",
    senderPrincipalId: alice, clientMessageId: extra.clientMessageId ?? id(), lifecycle: "ACTIVE", editCount: 0,
    createdAt: `2026-11-08T10:00:${String(sequence).padStart(2, "0")}Z`, attachmentIds: [], managed: { text: `m${sequence}` }, ...extra,
  };
}
const event = (eventSequence, type, data) => ({ type, conversationId: conv, eventSequence, data });

test("forward pages upsert by messageId and move the sequence cursor; MLS commits are skipped", () => {
  const st = new ManagedConversationState();
  const a = msg(1), b = msg(2);
  assert.equal(st.applyMessages([a, b, { ...msg(3), kind: "MLS_COMMIT" }]).size, 2);
  assert.equal(st.maxSequence, 3);
  assert.equal(st.minSequence, 1);
  assert.equal(st.applyMessages([a]).size, 0, "a duplicate changes nothing");
  assert.deepEqual(st.ordered().map((r) => r.text), ["m1", "m2"]);
});

test("an edit replaces the text in place; a stale (lower editCount) copy never overwrites it", () => {
  const st = new ManagedConversationState();
  const a = msg(1);
  st.applyMessages([a]);
  const edited = { ...a, managed: { text: "fixed" }, editCount: 1, editedAt: "2026-11-08T10:05:00Z" };
  assert.equal(st.applyMessages([edited]).size, 1);
  assert.equal(st.get(a.messageId).text, "fixed");
  assert.equal(st.applyMessages([a]).size, 0, "a stale page must not undo the edit");
  assert.equal(st.get(a.messageId).text, "fixed");
});

test("tombstones are final: content is dropped and a stale ACTIVE copy cannot resurrect it", () => {
  const st = new ManagedConversationState();
  const a = msg(1, { attachmentIds: ["bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"] });
  st.applyMessages([a]);
  st.applyMessages([{ ...a, lifecycle: "DELETED", managed: { text: null } }]);
  const r = st.get(a.messageId);
  assert.equal(r.lifecycle, "DELETED");
  assert.equal(r.text, null);
  assert.deepEqual(r.attachmentIds, []);
  st.applyMessages([a]);
  assert.equal(st.get(a.messageId).lifecycle, "DELETED");
  assert.equal(st.get(a.messageId).text, null);
});

test("events carry the current representation: edits and deletions apply anywhere in the window", () => {
  const st = new ManagedConversationState();
  const a = msg(1), b = msg(2);
  st.applyMessages([a, b]);
  const refresh = st.applyEvents([
    event(5, "message.edited", { message: { ...a, managed: { text: "edited" }, editCount: 1 } }),
    event(6, "message.deleted", { message: { ...b, lifecycle: "DELETED", managed: { text: null } } }),
  ]);
  assert.equal(st.get(a.messageId).text, "edited");
  assert.equal(st.get(b.messageId).lifecycle, "DELETED");
  assert.deepEqual([...refresh.messages].sort(), [a.messageId, b.messageId].sort());
  assert.equal(st.eventCursor, 6);
});

test("a deletion event without a hydrated message still tombstones what we hold", () => {
  const st = new ManagedConversationState();
  const a = msg(1);
  st.applyMessages([a]);
  st.applyEvents([event(1, "message.moderated", { messageId: a.messageId })]);
  assert.equal(st.get(a.messageId).lifecycle, "MODERATED");
  assert.equal(st.get(a.messageId).text, null);
});

test("events at or below the cursor are duplicates; out-of-order input is applied in order", () => {
  const st = new ManagedConversationState();
  const a = msg(1);
  st.applyMessages([a]);
  st.eventCursor = 10;
  st.applyEvents([event(10, "message.edited", { message: { ...a, managed: { text: "old replay" }, editCount: 5 } })]);
  assert.equal(st.get(a.messageId).text, "m1");
  st.applyEvents([
    event(12, "message.edited", { message: { ...a, managed: { text: "second" }, editCount: 2 } }),
    event(11, "message.edited", { message: { ...a, managed: { text: "first" }, editCount: 1 } }),
  ]);
  assert.equal(st.get(a.messageId).text, "second");
  assert.equal(st.eventCursor, 12);
});

test("new messages arrive by event; reactions, pins, membership and receipts ask for refetches", () => {
  const st = new ManagedConversationState();
  const a = msg(1);
  const refresh = emptyRefresh();
  st.applyEvents([
    event(1, "message.created", { message: a }),
    event(2, "reaction.added", { message: a }),
    event(3, "pin.added", { messageId: a.messageId }),
    event(4, "member.added", { principalId: bob }),
    event(5, "read.cursor.updated", { principalId: bob, readSequence: 1 }),
  ], refresh);
  assert.equal(st.get(a.messageId).text, "m1");
  assert.ok(refresh.reactions.has(a.messageId));
  assert.equal(refresh.pins, true);
  assert.equal(refresh.conversation, true);
  assert.deepEqual(st.seenBy(1, alice), [bob]);
  assert.deepEqual(st.seenBy(2, alice), []);
});

test("back-pagination extends the window downward and reports whether more exists", () => {
  const st = new ManagedConversationState();
  st.applyOlderPage([msg(10), msg(9)], 9);
  assert.equal(st.hasOlder, true);
  st.applyOlderPage([msg(8)], null);
  assert.equal(st.hasOlder, false);
  assert.equal(st.minSequence, 8);
  assert.deepEqual(st.ordered().map((r) => r.sequence), [8, 9, 10]);
});

test("clear-for-me hides messages at or below the watermark", () => {
  const st = new ManagedConversationState();
  st.applyMessages([msg(1), msg(2), msg(3)]);
  st.clearedThroughSequence = 2;
  assert.deepEqual(st.ordered().map((r) => r.sequence), [3]);
});

test("legacy EDIT records apply to their target and are never shown", () => {
  const st = new ManagedConversationState();
  const a = msg(1);
  st.applyMessages([a, { ...msg(2), kind: "EDIT", targetMessageId: a.messageId, managed: { text: "via edit record" } }]);
  assert.equal(st.ordered().length, 1);
  assert.equal(st.get(a.messageId).text, "via edit record");
  assert.equal(st.get(a.messageId).editCount, 1);
});

test("timeline cards merge by anchorSequence, then occurredAt, then entryId; sending messages stay last", () => {
  const messages = [
    { key: "m1", sequence: 1, createdAt: "2026-11-08T10:00:01Z" },
    { key: "m2", sequence: 2, createdAt: "2026-11-08T10:00:02Z" },
    { key: "q", sequence: null, createdAt: "2026-11-08T10:00:09Z" },
  ];
  const cards = [
    { entryId: "b", kind: "CALL", occurredAt: "2026-11-08T10:00:03Z", anchorSequence: 1 },
    { entryId: "a", kind: "CALL", occurredAt: "2026-11-08T10:00:03Z", anchorSequence: 1 },
    { entryId: "c", kind: "MEETING_INVITATION", occurredAt: "2026-11-08T10:00:05Z", anchorSequence: null },
  ];
  const order = mergeTimeline(messages, cards).map((i) => i.key);
  assert.deepEqual(order, ["m1", "card:a", "card:b", "m2", "card:c", "q"]);
  assert.deepEqual(mergeTimeline(messages, cards, 1).map((i) => i.key), ["m1", "m2", "card:c", "q"], "cards under the watermark are hidden");
});
