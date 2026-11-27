// Non-blocking status publishing: the queue engine with a fake transport and in-memory "device"
// storage — ordering, idempotency keys, resumable parts, back-off, offline, cancel, recovery
// after a reload, concurrency limits and no duplicate after an ambiguous failure.
// Run: node --experimental-strip-types --test tests/status-publisher.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { StatusQueue, backoffMs, transportError, MAX_AUTO_ATTEMPTS } from "../src/lib/status-publisher/queue.ts";
import { MemoryQueueStorage } from "../src/lib/status-publisher/storage.ts";

const blob = (bytes, type = "image/jpeg") => new Blob(["x".repeat(bytes)], { type });
const PART = 4;

/** A fake server behind the StatusTransport interface. */
function fakeServer({ listParts = true, idempotentCreate = true } = {}) {
  const uploads = new Map();
  const statuses = [];
  const byKey = new Map();
  const calls = { start: 0, parts: [], creates: [], cancelled: [], complete: 0, inflightParts: 0, maxInflightParts: 0, inflightUploads: new Set(), maxInflightUploads: 0 };
  const hooks = { part: null, create: null, afterCreate: null, complete: null };
  let n = 0;
  const transport = {
    capabilities: { listParts, idempotentCreate, cancelUpload: true },
    async startUpload(req) {
      calls.start++;
      const uploadId = `m${++n}`;
      uploads.set(uploadId, { size: req.sizeBytes, parts: new Set(), count: Math.ceil(req.sizeBytes / PART), req });
      return { uploadId, partSize: PART, partCount: Math.ceil(req.sizeBytes / PART) };
    },
    async storedParts(id) {
      const u = uploads.get(id);
      if (!u) throw transportError("gone", "gone", 404);
      return [...u.parts];
    },
    async uploadPart(id, index, body, signal, onProgress) {
      calls.inflightParts++;
      calls.maxInflightParts = Math.max(calls.maxInflightParts, calls.inflightParts);
      calls.inflightUploads.add(id);
      calls.maxInflightUploads = Math.max(calls.maxInflightUploads, calls.inflightUploads.size);
      try {
        await new Promise((r) => setTimeout(r, 1));
        if (hooks.part) await hooks.part(id, index);
        if (signal.aborted) throw transportError("aborted", "aborted");
        const u = uploads.get(id);
        if (!u) throw transportError("gone", "gone", 404);
        onProgress?.(body.size);
        u.parts.add(index);
        calls.parts.push(`${id}:${index}`);
      } finally {
        calls.inflightParts--;
        calls.inflightUploads.delete(id);
      }
    },
    async completeUpload(id) {
      calls.complete++;
      if (hooks.complete) await hooks.complete(id);
      const u = uploads.get(id);
      if (!u) throw transportError("gone", "gone", 404);
      if (u.parts.size !== u.count) throw transportError("gone", "missing pieces", 400);
      u.done = true;
      return { mediaId: id };
    },
    async cancelUpload(id) { calls.cancelled.push(id); uploads.delete(id); },
    async createStatus(req, key) {
      calls.creates.push({ req, key });
      if (hooks.create) await hooks.create(req, key);
      if (idempotentCreate && byKey.has(key)) return { postId: byKey.get(key).postId };
      if (req.mediaId && !uploads.get(req.mediaId)?.done) throw transportError("rejected", "media not complete", 400);
      const post = { postId: `p${statuses.length + 1}`, req, key, createdAt: new Date().toISOString() };
      statuses.push(post);
      byKey.set(key, post);
      if (hooks.afterCreate) await hooks.afterCreate(req, key);
      return { postId: post.postId };
    },
    async findCreatedStatus(req) {
      return statuses.filter((s) => s.req.kind === req.kind && (req.kind === "TEXT" ? s.req.text === req.text : s.req.mediaId === req.mediaId && s.req.startMs === req.startMs)).map((s) => ({ postId: s.postId }));
    },
  };
  return { transport, statuses, calls, hooks, uploads };
}

function makeQueue(server, storage = new MemoryQueueStorage(), extra = {}) {
  const clock = { now: 1_000_000 };
  const q = new StatusQueue({
    storage, transport: server.transport, now: () => clock.now, random: () => 0.5,
    timers: { set: () => 0, clear: () => {} }, ...extra,
  });
  return { q, clock, storage };
}

test("a multi-item job publishes in order with one groupId and increasing sequences; captions on the first segment", async () => {
  const server = fakeServer();
  const { q } = makeQueue(server);
  await q.start("me");
  const events = [];
  q.onEvent((e) => events.push(e));
  await q.enqueue({ items: [
    { kind: "TEXT", text: "hello", background: "#123456", font: 2 },
    { kind: "VIDEO", segments: [{ blob: blob(10, "video/mp4"), durationMs: 60000 }, { blob: blob(9, "video/mp4"), durationMs: 60000 }, { blob: blob(3, "video/mp4"), durationMs: 12000 }], caption: "  trip " },
    { kind: "IMAGE", blob: blob(6), caption: "pic" },
  ] });
  await q.settled();
  const reqs = server.statuses.map((s) => s.req);
  assert.deepEqual(reqs.map((r) => r.kind), ["TEXT", "VIDEO", "VIDEO", "VIDEO", "IMAGE"]);
  assert.deepEqual(reqs.map((r) => r.sequence), [0, 1, 2, 3, 4]);
  assert.equal(new Set(reqs.map((r) => r.groupId)).size, 1);
  assert.deepEqual(reqs.slice(1, 4).map((r) => r.caption), ["trip", undefined, undefined]);
  assert.deepEqual(reqs.slice(1, 4).map((r) => r.endMs), [60000, 60000, 12000]);
  assert.equal(reqs[4].caption, "pic");
  assert.equal(events.length, 1);
  assert.equal(events[0].type, "published");
  const snap = q.getSnapshot();
  assert.equal(snap.jobs[0].state, "done");
  q.acknowledge(events[0].job.id);
  assert.equal(q.getSnapshot().jobs.length, 0);
});

test("one source posted as consecutive ranges (today's server model): uploaded once, ranges in order", async () => {
  const server = fakeServer();
  const { q } = makeQueue(server);
  await q.start("me");
  await q.enqueue({ items: [{ kind: "VIDEO", source: blob(13, "video/mp4"), durationMs: 150000, ranges: [{ startMs: 0, endMs: 60000 }, { startMs: 60000, endMs: 120000 }, { startMs: 120000, endMs: 150000 }] }] });
  await q.settled();
  assert.equal(server.calls.start, 1);
  assert.deepEqual(server.statuses.map((s) => [s.req.startMs, s.req.endMs, s.req.mediaId]), [[0, 60000, "m1"], [60000, 120000, "m1"], [120000, 150000, "m1"]]);
});

test("statuses are published in enqueue order across jobs, even when a later job uploads first", async () => {
  const server = fakeServer();
  const { q } = makeQueue(server);
  await q.start("me");
  let releaseBig;
  const bigGate = new Promise((r) => { releaseBig = r; });
  server.hooks.part = async (id) => { if (id === "m1") await bigGate; };
  await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(12) }] });
  await q.enqueue({ items: [{ kind: "TEXT", text: "second" }] });
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(server.statuses.length, 0, "the text waits for the earlier photo");
  releaseBig();
  await q.settled();
  assert.deepEqual(server.statuses.map((s) => s.req.kind), ["IMAGE", "TEXT"]);
});

test("the idempotency key is generated once and reused by every retry", async () => {
  const server = fakeServer({ idempotentCreate: true });
  const { q, clock } = makeQueue(server);
  await q.start("me");
  // The server creates it but the answer is lost; the retry then dies on the way; the third gets the replay.
  let lose = 1;
  server.hooks.afterCreate = async () => { if (lose-- > 0) throw transportError("network", "timeout"); };
  let calls = 0;
  server.hooks.create = async () => { if (++calls === 2) throw transportError("server", "502", 502); };
  await q.enqueue({ items: [{ kind: "TEXT", text: "hi" }] });
  await q.settled();
  assert.equal(q.getSnapshot().jobs[0].state, "waiting");
  for (let i = 0; i < 3; i++) { clock.now += 120_000; q.kick(); await q.settled(); }
  const keys = server.calls.creates.map((c) => c.key);
  assert.equal(keys.length, 3);
  assert.equal(new Set(keys).size, 1, "same key every time");
  assert.equal(server.statuses.length, 1, "created once");
  assert.equal(q.getSnapshot().jobs[0].state, "done");
});

test("without server idempotency, an ambiguous create is checked before re-sending: no duplicate", async () => {
  const server = fakeServer({ idempotentCreate: false });
  const { q, clock } = makeQueue(server);
  await q.start("me");
  let lose = 1;
  server.hooks.afterCreate = async () => { if (lose-- > 0) throw transportError("network", "timeout"); };
  await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(5) }, { kind: "TEXT", text: "same" }, { kind: "TEXT", text: "same" }] });
  await q.settled();
  clock.now += 120_000; q.kick(); await q.settled();
  assert.deepEqual(server.statuses.map((s) => s.req.kind), ["IMAGE", "TEXT", "TEXT"], "the photo once, both identical texts kept");
  assert.equal(server.calls.creates.length, 3, "the lost photo create was not re-sent");
});

test("a definite refusal is not treated as possibly-created", async () => {
  const server = fakeServer({ idempotentCreate: false });
  const { q } = makeQueue(server);
  await q.start("me");
  server.hooks.create = async () => { throw transportError("rejected", "Text too long", 400); };
  await q.enqueue({ items: [{ kind: "TEXT", text: "x" }] });
  await q.settled();
  const job = q.getSnapshot().jobs[0];
  assert.equal(job.state, "failed");
  assert.equal(job.error, "Text too long");
});

test("transient failures back off exponentially (with jitter), then fail; Retry resumes", async () => {
  assert.equal(backoffMs(1, () => 0), 500);
  assert.equal(backoffMs(1, () => 1), 1000);
  assert.equal(backoffMs(3, () => 1), 4000);
  assert.equal(backoffMs(30, () => 1), 60000);
  const server = fakeServer();
  const { q, clock } = makeQueue(server);
  await q.start("me");
  const events = [];
  q.onEvent((e) => events.push(e.type));
  let down = true;
  server.hooks.part = async () => { if (down) throw transportError("server", "503", 503); };
  await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(8) }] });
  await q.settled();
  for (let i = 1; i < MAX_AUTO_ATTEMPTS; i++) {
    assert.equal(q.getSnapshot().jobs[0].state, "waiting");
    clock.now += 1; q.kick(); await q.settled();
    assert.equal(server.calls.parts.length, 0, "not retried before the back-off ends");
    clock.now += 70_000; q.kick(); await q.settled();
  }
  const job = q.getSnapshot().jobs[0];
  assert.equal(job.state, "failed");
  assert.equal(job.error, "Failed to upload");
  assert.deepEqual(events, ["failed"]);
  down = false;
  q.retry(job.id);
  await q.settled();
  assert.equal(q.getSnapshot().jobs[0].state, "done");
  assert.equal(server.statuses.length, 1);
});

test("offline: nothing is attempted or counted; online resumes", async () => {
  const server = fakeServer();
  const { q } = makeQueue(server);
  await q.start("me");
  q.setOnline(false);
  await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(8) }] });
  await q.settled();
  assert.equal(server.calls.start, 0);
  assert.equal(q.getSnapshot().jobs[0].state, "waiting");
  assert.equal(q.getSnapshot().online, false);
  // Connection drops mid-upload: the failure isn't counted.
  q.setOnline(true);
  server.hooks.part = async () => { q.setOnline(false); throw transportError("network", "offline"); };
  await q.settled();
  assert.equal(q.getSnapshot().jobs[0].state, "waiting");
  server.hooks.part = null;
  q.setOnline(true);
  await q.settled();
  assert.equal(q.getSnapshot().jobs[0].state, "done");
});

test("resume from stored parts after a reload: only missing parts are sent, the upload isn't restarted", async () => {
  const server = fakeServer({ listParts: true });
  const storage = new MemoryQueueStorage();
  const first = makeQueue(server, storage);
  await first.q.start("me");
  let count = 0;
  // Parts 0-2 arrive; then the tab dies (everything after aborts).
  server.hooks.part = async () => { if (++count > 3) throw transportError("network", "dead"); };
  await first.q.enqueue({ items: [{ kind: "VIDEO", segments: [{ blob: blob(20, "video/mp4"), durationMs: 5000 }] }] });
  await first.q.settled();
  first.q.stop();
  const sentBefore = server.calls.parts.length;
  assert.ok(sentBefore >= 1 && sentBefore <= 3);
  // The server even got one more part the tab never heard about.
  server.uploads.get("m1").parts.add(4);
  server.hooks.part = null;
  const second = makeQueue(server, storage);
  await second.q.start("me");
  await second.q.settled();
  assert.equal(server.calls.start, 1, "same upload resumed");
  const resent = server.calls.parts.slice(sentBefore);
  const already = new Set([...server.calls.parts.slice(0, sentBefore).map((p) => Number(p.split(":")[1])), 4]);
  for (const p of resent) assert.ok(!already.has(Number(p.split(":")[1])), `part ${p} was re-sent`);
  assert.equal(server.statuses.length, 1);
  assert.equal(storage.jobs.size, 0, "erased once published");
  assert.equal(storage.blobs.size, 0);
});

test("an upload the server lost is restarted (that upload only)", async () => {
  const server = fakeServer();
  const { q, clock } = makeQueue(server);
  await q.start("me");
  let once = true;
  server.hooks.complete = async (id) => { if (once) { once = false; server.uploads.delete(id); } };
  await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(8) }] });
  await q.settled();
  clock.now += 120_000; q.kick(); await q.settled();
  assert.equal(server.calls.start, 2);
  assert.equal(server.statuses.length, 1);
  assert.equal(server.statuses[0].req.mediaId, "m2");
});

test("persistence: a job queued offline survives a reload and is published by the next start", async () => {
  const server = fakeServer();
  const storage = new MemoryQueueStorage();
  const first = makeQueue(server, storage);
  await first.q.start("me");
  first.q.setOnline(false);
  const id = await first.q.enqueue({ items: [{ kind: "IMAGE", blob: blob(9), caption: "kept" }, { kind: "TEXT", text: "after" }] });
  first.q.stop();
  assert.equal(storage.jobs.size, 1);
  assert.ok([...storage.blobs.keys()].every((k) => k.startsWith(`${id}/`)));
  const keysBefore = storage.jobs.get(id).posts.map((p) => p.idempotencyKey);

  // Another account signing in on this browser doesn't see (or publish) it.
  const other = makeQueue(server, storage);
  await other.q.start("someone-else");
  await other.q.settled();
  assert.equal(other.q.getSnapshot().jobs.length, 0);
  assert.equal(server.calls.start, 0);
  other.q.stop();

  const second = makeQueue(server, storage);
  await second.q.start("me");
  const snap = second.q.getSnapshot();
  assert.equal(snap.jobs.length, 1);
  assert.equal(snap.jobs[0].preview.kind, "TEXT");
  await second.q.settled();
  assert.deepEqual(server.statuses.map((s) => [s.req.kind, s.req.caption ?? s.req.text]), [["IMAGE", "kept"], ["TEXT", "after"]]);
  assert.deepEqual(server.calls.creates.map((c) => c.key), keysBefore, "keys persisted with the job");
});

test("an image job previews its photo; a reload restores the preview blob", async () => {
  const server = fakeServer();
  const storage = new MemoryQueueStorage();
  const first = makeQueue(server, storage);
  await first.q.start("me");
  first.q.setOnline(false);
  const photo = blob(7);
  await first.q.enqueue({ items: [{ kind: "IMAGE", blob: photo }] });
  assert.equal(first.q.getSnapshot().jobs[0].preview.blob, photo);
  first.q.stop();
  const second = makeQueue(server, storage);
  await second.q.start("me");
  assert.equal(second.q.getSnapshot().jobs[0].preview.blob?.size, 7);
});

test("cancel aborts the upload, discards it server-side, erases local data and never creates", async () => {
  const server = fakeServer();
  const { q, storage } = makeQueue(server);
  await q.start("me");
  let gate;
  const blocked = new Promise((r) => { gate = r; });
  let started;
  const isStarted = new Promise((r) => { started = r; });
  server.hooks.part = async () => { started(); await blocked; };
  const id = await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(12) }] });
  await isStarted;
  await q.cancel(id);
  gate();
  await q.settled();
  assert.deepEqual(server.calls.cancelled, ["m1"]);
  assert.equal(server.calls.creates.length, 0);
  assert.equal(q.getSnapshot().jobs.length, 0);
  assert.equal(storage.jobs.size, 0);
  assert.equal(storage.blobs.size, 0);
});

test("concurrency: at most 2 jobs upload at once, at most 3 parts per upload", async () => {
  const server = fakeServer();
  const { q } = makeQueue(server, undefined, { maxActiveJobs: 2, partConcurrency: 3 });
  await q.start("me");
  for (let i = 0; i < 4; i++) await q.enqueue({ items: [{ kind: "IMAGE", blob: blob(40) }] });
  await q.settled();
  assert.equal(server.statuses.length, 4);
  assert.ok(server.calls.maxInflightUploads <= 2, `uploads in flight: ${server.calls.maxInflightUploads}`);
  assert.ok(server.calls.maxInflightParts <= 6, `parts in flight: ${server.calls.maxInflightParts}`);
  assert.ok(server.calls.maxInflightParts >= 3, "parts do go in parallel");
  assert.deepEqual(server.statuses.map((s) => s.req.mediaId), ["m1", "m2", "m3", "m4"], "published in enqueue order");
});

test("progress grows across processing, upload and publish", async () => {
  const server = fakeServer();
  const { q } = makeQueue(server);
  await q.start("me");
  const seen = [];
  q.subscribe(() => { const j = q.getSnapshot().jobs[0]; if (j) seen.push(j.progress); });
  await q.enqueue({ items: [{ kind: "VIDEO", process: { processor: "trim", input: blob(30, "video/mp4"), params: { cut: [0, 1] } } }] });
  q.registerProcessor("trim", async (input, params, ctx) => {
    assert.deepEqual(params, { cut: [0, 1] });
    ctx.onProgress(0.5);
    return { segments: [{ blob: input.slice(0, 10, "video/mp4"), durationMs: 60000 }, { blob: input.slice(10, 18, "video/mp4"), durationMs: 30000 }] };
  });
  await q.settled();
  assert.deepEqual(server.statuses.map((s) => [s.req.sequence, s.req.endMs]), [[0, 60000], [1, 30000]]);
  for (let i = 1; i < seen.length; i++) assert.ok(seen[i] >= seen[i - 1] - 1e-9, `progress went back: ${seen}`);
  assert.equal(seen[seen.length - 1], 1);
});

test("deferred processing: a named processor re-runs after a reload; a closure can't and fails (cancel only)", async () => {
  const server = fakeServer();
  const storage = new MemoryQueueStorage();
  const first = makeQueue(server, storage);
  await first.q.start("me");
  first.q.setOnline(false);
  await first.q.enqueue({ items: [{ kind: "TEXT", text: "before" }, { kind: "VIDEO", process: { processor: "edit", input: blob(8, "video/mp4"), params: { speed: 2 } }, caption: "c" }, { kind: "TEXT", text: "after" }] });
  await first.q.enqueue({ items: [{ kind: "VIDEO", prepare: async () => ({ segments: [{ blob: blob(4, "video/mp4"), durationMs: 1000 }] }) }] });
  first.q.stop();

  const second = makeQueue(server, storage);
  await second.q.start("me");
  await second.q.settled();
  const jobs = second.q.getSnapshot().jobs;
  // Named processor not registered yet: the first job waits (not fatal); the closure job is fatal.
  assert.equal(jobs.find((j) => j.postCount === 3).state, "failed");
  assert.equal(jobs.find((j) => j.postCount === 3).fatal, false);
  assert.equal(jobs.find((j) => j.postCount === 1).fatal, true);
  assert.deepEqual(server.statuses.map((s) => s.req.text), ["before"], "items before the deferred one can go");
  second.q.registerProcessor("edit", async (input, params) => {
    assert.deepEqual(params, { speed: 2 });
    return { segments: [{ blob: input, durationMs: 4000 }] };
  });
  await second.q.settled();
  assert.deepEqual(server.statuses.map((s) => [s.req.kind, s.req.sequence, s.req.caption ?? s.req.text]), [["TEXT", 0, "before"], ["VIDEO", 1, "c"], ["TEXT", 2, "after"]]);
  const fatal = second.q.getSnapshot().jobs.find((j) => j.state === "failed");
  second.q.retry(fatal.id);
  assert.equal(second.q.getSnapshot().jobs.find((j) => j.id === fatal.id).state, "failed", "retry can't help a fatal job");
  await second.q.cancel(fatal.id);
  assert.equal(storage.jobs.size, 0);
});

test("a follower tab mirrors storage and never works the queue; it takes over when it becomes the leader", async () => {
  const server = fakeServer();
  const storage = new MemoryQueueStorage();
  const leader = makeQueue(server, storage);
  await leader.q.start("me");
  leader.q.setOnline(false);
  await leader.q.enqueue({ items: [{ kind: "TEXT", text: "from leader" }] });
  const follower = makeQueue(server, storage);
  await follower.q.start("me", false);
  await follower.q.enqueue({ items: [{ kind: "TEXT", text: "from follower" }] });
  await follower.q.settled();
  assert.equal(follower.q.getSnapshot().jobs.length, 2);
  assert.equal(server.calls.creates.length, 0);
  // The leader tab closes; the follower is elected.
  leader.q.stop();
  await follower.q.setActive(true);
  await follower.q.settled();
  assert.deepEqual(server.statuses.map((s) => s.req.text), ["from leader", "from follower"]);
});

test("a job finished here is never re-adopted from a stale storage read", async () => {
  const server = fakeServer();
  const storage = new MemoryQueueStorage();
  const { q } = makeQueue(server, storage);
  await q.start("me");
  q.setOnline(false);
  await q.enqueue({ items: [{ kind: "TEXT", text: "once" }] });
  const stale = await storage.loadJobs();
  q.setOnline(true);
  await q.settled();
  for (const j of stale) await storage.putJob(j);
  await q.reload();
  await q.settled();
  assert.equal(server.statuses.length, 1);
});
