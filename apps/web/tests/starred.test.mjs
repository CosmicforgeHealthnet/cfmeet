// Unified starred content (brief §19; ADR 0074): chat and group stars from the Hub's saved
// messages, channel posts from the channel store; unavailable items never shown; search,
// filters, jump links and remove handles.
import assert from "node:assert/strict";
import test from "node:test";
import { arrangeStarred, channelItems, channelPostHref, messageHref, messageItems, starredChips, visibleSaved } from "../src/lib/starred.ts";

const saved = [
  { savedId: "s1", conversationId: "c1", messageId: "m1", savedAt: "2026-11-28T10:00:00Z", available: true },
  { savedId: "s2", conversationId: "g1", messageId: "m2", savedAt: "2026-11-28T11:00:00Z", available: true },
  { savedId: "s3", conversationId: "c1", messageId: "m3", savedAt: "2026-11-28T12:00:00Z", available: false, unavailableReason: "DELETED" },
  { savedId: "s4", conversationId: "left", messageId: "m4", savedAt: "2026-11-28T12:30:00Z", available: false, unavailableReason: "NOT_A_MEMBER" },
  { savedId: "s5", conversationId: "gone", messageId: "m5", savedAt: "2026-11-28T09:00:00Z", available: true },
];
const chats = { c1: { title: "Ada", group: false }, g1: { title: "Family", group: true } };
const texts = { m1: "See you at 6", m2: null };

test("deleted, expired, hidden or left items never come back", () => {
  assert.deepEqual(visibleSaved(saved).map((s) => s.savedId), ["s1", "s2", "s5"]);
  const items = messageItems(saved, (c) => chats[c] ?? null, (_c, m) => texts[m] ?? null);
  // s5's chat is not in the account's list any more: not shown either.
  assert.deepEqual(items.map((i) => i.key), ["saved:s1", "saved:s2"]);
  assert.equal(items[0].source, "chat");
  assert.equal(items[1].source, "group");
  assert.equal(items[1].text, null, "a message not readable here yet has no text");
  assert.deepEqual(items[0].removeRef, { kind: "saved", savedId: "s1" });
  assert.equal(items[0].href, messageHref("c1", "m1"));
});

test("jump links open the original message or post", () => {
  assert.equal(messageHref("c 1", "m/1"), "/dashboard/messages?conversation=c%201&message=m%2F1");
  assert.equal(channelPostHref("ch1", "p1"), "/dashboard/updates/channels/ch1?post=p1");
});

test("channel stars become items with a readable label for media posts", () => {
  const items = channelItems([
    { starredAt: "2026-11-28T13:00:00Z", channel: { channelId: "ch1", name: "CF News" }, post: { postId: "p1", text: null, kind: "IMAGE" } },
    { starredAt: "2026-11-28T08:00:00Z", channel: { channelId: "ch1", name: "CF News" }, post: { postId: "p2", text: "Launch day" } },
  ]);
  assert.deepEqual(items.map((i) => [i.source, i.text]), [["channel", "Photo"], ["channel", "Launch day"]]);
  assert.deepEqual(items[0].removeRef, { kind: "channel", channelId: "ch1", postId: "p1" });
});

test("search and filters apply to name and text, newest star first", () => {
  const all = [
    ...messageItems(saved, (c) => chats[c] ?? null, (_c, m) => texts[m] ?? null),
    ...channelItems([{ starredAt: "2026-11-28T13:00:00Z", channel: { channelId: "ch1", name: "CF News" }, post: { postId: "p1", text: "Launch at 6" } }]),
  ];
  assert.deepEqual(arrangeStarred(all, "all", "").map((i) => i.key), ["channel:p1", "saved:s2", "saved:s1"]);
  assert.deepEqual(arrangeStarred(all, "all", "at 6").map((i) => i.key), ["channel:p1", "saved:s1"]);
  assert.deepEqual(arrangeStarred(all, "group", "").map((i) => i.key), ["saved:s2"]);
  assert.deepEqual(arrangeStarred(all, "chat", "family").map((i) => i.key), []);
  assert.deepEqual(arrangeStarred(all, "all", "FAMILY").map((i) => i.key), ["saved:s2"]);
  assert.deepEqual(starredChips(all, true).map((c) => [c.key, c.count]), [["all", 3], ["chat", 1], ["group", 1], ["channel", 1]]);
  assert.deepEqual(starredChips(all, false).map((c) => c.key), ["all", "chat", "group"]);
});
