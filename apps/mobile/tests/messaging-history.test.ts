import { test } from "node:test";
import assert from "node:assert/strict";
import { collectConversationPages, readConversationHistory } from "../lib/messaging/history.ts";
import type { ServerMessage } from "../lib/messaging/relay-paths.ts";

function message(sequence: number, kind = "APPLICATION"): ServerMessage {
  return {
    messageId: `m${sequence}`, conversationId: "c", sequence, kind,
    securityProfile: "MANAGED", senderPrincipalId: "me", clientMessageId: `c${sequence}`,
    lifecycle: "ACTIVE", createdAt: "2026-11-25T00:00:00Z", managed: { text: `text${sequence}` },
  };
}

test("conversation pagination includes more than the former five-page limit", async () => {
  const values = await collectConversationPages(async (cursor) => {
    const n = cursor ? Number(cursor) : 0;
    return { conversations: [n], nextCursor: n < 11 ? String(n + 1) : null };
  });
  assert.deepEqual(values, Array.from({ length: 12 }, (_, n) => n));
});

test("cyclic conversation cursors fail visibly instead of truncating or looping", async () => {
  await assert.rejects(collectConversationPages(async () => ({ conversations: [], nextCursor: "same" })), /did not advance/);
});

test("cache eviction or logout replays history from zero instead of retaining a stale global cursor", async () => {
  const requested: number[] = [];
  const fetchPage = async (after: number) => {
    requested.push(after);
    return { messages: after === 0 ? [message(1)] : [] };
  };
  const first = await readConversationHistory(undefined, "me", fetchPage);
  await readConversationHistory(first, "me", fetchPage);
  const restored = await readConversationHistory(undefined, "me", fetchPage);
  assert.deepEqual(requested, [0, 1, 0]);
  assert.deepEqual(restored, first);
});

test("interrupted pagination does not publish a cursor past history absent from the cache", async () => {
  const previous = { lines: [], afterSequence: 0 };
  await assert.rejects(readConversationHistory(previous, "me", async (after) => {
    if (after) throw new Error("offline");
    return { messages: Array.from({ length: 50 }, (_, n) => message(n + 1)) };
  }), /offline/);
  assert.deepEqual(previous, { lines: [], afterSequence: 0 });
});

test("large history resumes from bounded batches and advances over non-display events", async () => {
  const fetchPage = async (after: number) => ({
    messages: Array.from({ length: Math.min(50, 1051 - after) }, (_, n) => message(after + n + 1, "MLS_COMMIT")),
  });
  const batch = await readConversationHistory(undefined, "me", fetchPage);
  assert.equal(batch.afterSequence, 1000);
  const complete = await readConversationHistory(batch, "me", fetchPage);
  assert.equal(complete.afterSequence, 1051);
  assert.deepEqual(complete.lines, []);
});

test("nonadvancing message pages fail visibly", async () => {
  await assert.rejects(readConversationHistory({ lines: [], afterSequence: 5 }, "me", async () => ({ messages: [message(5)] })), /did not advance/);
});
