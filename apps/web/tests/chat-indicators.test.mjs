// Chat-list indicators (brief §3, §18; ADR 0073): status replies and shares with their
// context, calls (voice / video / missed) as the latest activity, reactions to your status,
// group previews before the first message, and list order that counts calls and reactions.
import assert from "node:assert/strict";
import test from "node:test";
import {
  activityAt, callPreview, groupActivityPreview, latestReactionBy, orderByActivity, rowOverlay, statusMessage, statusMessagePreview,
} from "../src/lib/messaging/chat-indicators.ts";

test("a status reply is recognised with what the status was and the reply text", () => {
  assert.deepEqual(statusMessage("Replying to your status (photo): Lovely view"), { kind: "status-reply", what: "photo", text: "Lovely view" });
  assert.deepEqual(statusMessage("Replying to your status (“Good morning”): Same to you: really"), { kind: "status-reply", what: "“Good morning”", text: "Same to you: really" });
  assert.equal(statusMessage("Replying to your status (photo) no colon"), null);
  assert.equal(statusMessage("hello"), null);
  assert.equal(statusMessage(null), null);
});

test("a forwarded status is recognised only with a real status id", () => {
  const id = "0b9f6c1e-2d3a-4b5c-8d7e-6f5a4b3c2d44";
  assert.deepEqual(statusMessage(`Shared a status with you: cfmeet://status/${id}`), { kind: "status-share", statusId: id });
  assert.equal(statusMessage("Shared a status with you: cfmeet://status/not-an-id"), null);
  assert.equal(statusMessage(`Look: cfmeet://status/${id}`), null);
});

test("the row line says whose reply or share it is", () => {
  const reply = statusMessage("Replying to your status (video): 😂");
  assert.deepEqual(statusMessagePreview(reply, false), { context: "Replied to your status · video", text: "😂" });
  assert.deepEqual(statusMessagePreview(reply, true), { context: "You replied to a status", text: "😂" });
  const share = statusMessage("Shared a status with you: cfmeet://status/0b9f6c1e-2d3a-4b5c-8d7e-6f5a4b3c2d44");
  assert.equal(statusMessagePreview(share, false).text, "Shared a status with you");
  assert.equal(statusMessagePreview(share, true).text, "You shared a status");
});

test("a call card becomes a voice or video call preview, missed in its own words", () => {
  assert.deepEqual(callPreview({ kind: "CALL", occurredAt: "2026-11-28T10:00:00Z", missed: true, callKind: "VIDEO" }), { video: true, missed: true, label: "Missed video call", at: "2026-11-28T10:00:00Z" });
  assert.equal(callPreview({ kind: "CALL", occurredAt: "2026-11-28T10:00:00Z", missed: false, callKind: "VOICE" }).label, "Voice call");
  // An older Hub without callKind: a voice call (the safe default icon).
  assert.equal(callPreview({ kind: "CALL", occurredAt: "2026-11-28T10:00:00Z" }).label, "Voice call");
  assert.equal(callPreview({ kind: "MEETING_INVITATION", occurredAt: "2026-11-28T10:00:00Z" }), null);
  assert.equal(callPreview(null), null);
});

test("the latest reaction of one person to my live statuses", () => {
  const posts = [
    { viewers: [{ principalId: "b", reaction: "😍", reactedAt: "2026-11-28T09:00:00Z" }, { principalId: "c", reaction: null, reactedAt: null }] },
    { viewers: [{ principalId: "b", reaction: "🔥", reactedAt: "2026-11-28T11:00:00Z" }] },
    { viewers: [{ principalId: "b", reaction: "👍" }] },
  ];
  assert.deepEqual(latestReactionBy("b", posts), { emoji: "🔥", at: "2026-11-28T11:00:00Z" });
  assert.equal(latestReactionBy("c", posts), null);
  assert.equal(latestReactionBy(null, posts), null);
});

test("a call or reaction replaces the preview only when newer than the last message", () => {
  const call = callPreview({ kind: "CALL", occurredAt: "2026-11-28T10:00:00Z", missed: true });
  const reaction = { emoji: "😍", at: "2026-11-28T11:00:00Z" };
  assert.equal(rowOverlay({ lastMessageAt: "2026-11-28T12:00:00Z", call, reaction }), null);
  assert.deepEqual(rowOverlay({ lastMessageAt: "2026-11-28T09:00:00Z", call, reaction: null }), { kind: "call", call });
  assert.deepEqual(rowOverlay({ lastMessageAt: "2026-11-28T09:00:00Z", call, reaction }), { kind: "status-reaction", reaction });
  assert.deepEqual(rowOverlay({ lastMessageAt: null, call, reaction: null }), { kind: "call", call });
});

test("calls and reactions count as activity for the list order; ties keep the realtime order", () => {
  assert.equal(activityAt("2026-11-28T09:00:00Z", null, "2026-11-28T10:00:00Z"), "2026-11-28T10:00:00Z");
  assert.equal(activityAt("2026-11-28T09:00:00Z", "2026-11-28T08:00:00Z"), "2026-11-28T09:00:00Z");
  const chats = [
    { id: "a", lastActivityAt: "2026-11-28T12:00:00Z" },
    { id: "b", lastActivityAt: "2026-11-28T11:00:00Z" },
    { id: "c", lastActivityAt: "2026-11-28T11:00:00Z" },
  ];
  const calls = { c: "2026-11-28T13:00:00Z" };
  assert.deepEqual(orderByActivity(chats, (x) => [calls[x.id]]).map((x) => x.id), ["c", "a", "b"]);
  assert.deepEqual(orderByActivity(chats, () => []).map((x) => x.id), ["a", "b", "c"]);
});

test("a group without messages says who created it", () => {
  const g = { contextClass: "GROUP", ownerPrincipalId: "me", lastMessage: null };
  assert.equal(groupActivityPreview(g, false, "me", {}), "You created this group");
  assert.equal(groupActivityPreview({ ...g, ownerPrincipalId: "x" }, false, "me", { x: "Ada Lovelace" }), "Ada created this group");
  assert.equal(groupActivityPreview({ ...g, ownerPrincipalId: "x" }, false, "me", {}), "Group created");
  assert.equal(groupActivityPreview({ ...g, lastMessage: {} }, false, "me", {}), null);
  assert.equal(groupActivityPreview(g, true, "me", {}), null);
  assert.equal(groupActivityPreview({ ...g, contextClass: "DIRECT" }, false, "me", {}), null);
});
