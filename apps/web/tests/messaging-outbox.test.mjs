// MSG-008: the durable MANAGED offline queue — one clientMessageId per send for its whole life
// (idempotent retries), per-conversation order, back-off for transient failures, a stop on
// permanent refusals, and survival across reloads.
import assert from "node:assert/strict";
import test from "node:test";
import { MessageOutbox, flushOutbox, isRetryable, backoffMs } from "../src/lib/messaging/outbox.ts";

const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
function memory() {
  let value = null;
  return { load: () => value, save: (v) => { value = v; }, clear: () => { value = null; }, peek: () => value };
}
let n = 0;
const item = (conversationId, text) => ({ key: `k${++n}`, conversationId, clientMessageId: `c${n}`, text });

test("retries reuse the same clientMessageId; a transient failure backs off then succeeds", async () => {
  let now = 1000;
  const box = new MessageOutbox(memory(), () => now);
  box.enqueue(item(A, "hello"));
  const sent = [];
  let fail = true;
  const send = async (i) => {
    sent.push(i.clientMessageId);
    if (fail) throw { status: 0, code: "NETWORK_UNAVAILABLE" };
  };
  assert.equal(await flushOutbox(box, send), 0);
  assert.equal(box.all()[0].state, "queued");
  assert.ok(box.all()[0].nextAttemptAt > now, "backs off");
  assert.equal(await flushOutbox(box, send), 0, "not due yet");
  now += 60_000;
  fail = false;
  assert.equal(await flushOutbox(box, send), 1);
  assert.deepEqual(sent, [sent[0], sent[0]], "the same clientMessageId both times");
  assert.equal(box.all().length, 0);
});

test("per-conversation order: a later message never overtakes a waiting one", async () => {
  const box = new MessageOutbox(memory(), () => 0);
  box.enqueue(item(A, "first"));
  box.enqueue(item(A, "second"));
  box.enqueue(item(B, "other"));
  assert.deepEqual(box.due().map((i) => i.text), ["first", "other"]);
  const order = [];
  await flushOutbox(box, async (i) => { order.push(i.text); });
  assert.deepEqual(order, ["first", "other", "second"]);
});

test("a permanent refusal stops and blocks its conversation until retried or discarded", async () => {
  const box = new MessageOutbox(memory(), () => 0);
  box.enqueue(item(A, "refused"));
  box.enqueue(item(A, "after"));
  await flushOutbox(box, async (i) => { if (i.text === "refused") throw { status: 403, code: "PERMISSION_DENIED" }; });
  assert.equal(box.all()[0].state, "failed");
  assert.equal(box.all()[0].error, "PERMISSION_DENIED");
  assert.equal(box.due().length, 0, "order is kept: nothing after it leaves");
  box.discard(box.all()[0].key);
  assert.deepEqual(box.due().map((i) => i.text), ["after"]);
});

test("a reload re-queues an interrupted send with its original clientMessageId", () => {
  const storage = memory();
  const box = new MessageOutbox(storage, () => 0);
  const queued = box.enqueue(item(A, "in flight"));
  box.markSending(queued.key);
  const reloaded = new MessageOutbox(storage, () => 0);
  assert.equal(reloaded.all().length, 1);
  assert.equal(reloaded.all()[0].state, "queued");
  assert.equal(reloaded.all()[0].clientMessageId, queued.clientMessageId);
});

test("a sync that already holds the message confirms (removes) the queued copy", () => {
  const box = new MessageOutbox(memory(), () => 0);
  const q = box.enqueue(item(A, "x"));
  assert.equal(box.confirm(B, [q.clientMessageId]), 0, "other conversation");
  assert.equal(box.confirm(A, [q.clientMessageId]), 1);
  assert.equal(box.all().length, 0);
});

test("corrupt storage is ignored; wake makes backed-off items due; leaving drops them", () => {
  const storage = memory();
  storage.save("{not json");
  const box = new MessageOutbox(storage, () => 5);
  assert.equal(box.all().length, 0);
  const q = box.enqueue(item(A, "x"));
  box.markFailed(q.key, "HTTP_503", true);
  assert.equal(box.due().length, 0);
  box.wake();
  assert.equal(box.due().length, 1);
  box.dropConversation(A);
  assert.equal(storage.peek(), null, "an empty queue leaves nothing stored");
});

test("retry classification and back-off bounds", () => {
  assert.equal(isRetryable(0), true);
  assert.equal(isRetryable(429), true);
  assert.equal(isRetryable(503), true);
  assert.equal(isRetryable(400), false);
  assert.equal(isRetryable(409, "IDEMPOTENCY_KEY_REUSED"), false);
  assert.equal(backoffMs(1), 1000);
  assert.equal(backoffMs(30), 60_000);
});
