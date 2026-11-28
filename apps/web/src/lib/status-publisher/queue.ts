/**
 * Non-blocking status publishing (WhatsApp-style): the framework-free queue engine.
 *
 * * `enqueue(job)` returns as soon as the job is in memory (and, best effort, in storage): the
 *   editor closes at once and the job is processed, uploaded and published in the background.
 * * Every status creation carries an idempotency key generated ONCE when the job is enqueued and
 *   persisted with it, so a retry is never a duplicate. When the transport can't honour the key
 *   (`capabilities.idempotentCreate === false`), a create whose outcome is unknown is checked
 *   against the server (`findCreatedStatus`) before it is sent again.
 * * Uploads are resumable: acknowledged parts are recorded; after a reload the server is asked
 *   which parts it has (`storedParts`) and only the missing ones are sent. An upload the server
 *   no longer knows ("gone") is restarted — that one upload only.
 * * Ordering: statuses are published strictly in order — across jobs (enqueue order) and within
 *   a job (each item / segment has a sequence; N only after N-1). A failed job steps aside so
 *   later jobs are not stuck behind it.
 * * At most `maxActiveJobs` jobs process/upload at once, `partConcurrency` parts per upload;
 *   transient failures back off exponentially with jitter; nothing is attempted while offline;
 *   `cancel` aborts the work and discards the server-side upload when the transport can.
 * * Pending statuses exist only here (memory + IndexedDB) until the server confirms creation, so
 *   only the owner ever sees them.
 *
 * Pure (type-only imports): tests/status-publisher.test.mjs runs this file directly.
 */
import type {
  JobPreview,
  JobState,
  JobView,
  ProcessorResult,
  QueueEvent,
  QueueSnapshot,
  QueueStorage,
  StatusCreateRequest,
  StatusJobInput,
  StatusProcessor,
  StatusTransport,
  StoredJob,
  StoredPost,
  StoredUpload,
  TransportError,
  TransportErrorKind,
  ProcessorContext,
} from "./types";

export const MAX_AUTO_ATTEMPTS = 6;
const PROCESS_WEIGHT = 0.3;
const PUBLISH_WEIGHT = 0.1;
/** Clock skew tolerated when matching a possibly-created status on the server. */
const FIND_SKEW_MS = 5 * 60_000;

export function transportError(kind: TransportErrorKind, message: string, status?: number): TransportError {
  return { transportError: true, kind, message, status };
}

export function isTransportError(e: unknown): e is TransportError {
  return typeof e === "object" && e !== null && (e as { transportError?: unknown }).transportError === true;
}

/** Exponential back-off with jitter: ~1 s, 2 s, 4 s … capped at 60 s, each scaled into [50 %, 100 %]. */
export function backoffMs(attempt: number, random: () => number = Math.random): number {
  const base = Math.min(60_000, 1000 * 2 ** Math.max(0, attempt - 1));
  return Math.round(base / 2 + random() * (base / 2));
}

function defaultUuid(): string {
  const c = (globalThis as { crypto?: { randomUUID?: () => string } }).crypto;
  if (c?.randomUUID) return c.randomUUID();
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (ch) => {
    const r = (Math.random() * 16) | 0;
    return (ch === "x" ? r : (r & 0x3) | 0x8).toString(16);
  });
}

const contentTypeOf = (blob: Blob, kind: "IMAGE" | "VIDEO" | "AUDIO") =>
  (blob.type || (kind === "AUDIO" ? "audio/webm" : kind === "VIDEO" ? "video/mp4" : "image/jpeg")).split(";")[0];

/** Bytes of part `i` of an upload. */
const partBytes = (u: StoredUpload, i: number) => Math.max(0, Math.min(u.partSize ?? 0, u.sizeBytes - i * (u.partSize ?? 0)));

export interface Timers {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

export interface QueueOptions {
  storage: QueueStorage;
  transport: StatusTransport;
  /** Storage survives reloads (IndexedDB) — false for the in-memory fallback. */
  persistent?: boolean;
  now?: () => number;
  random?: () => number;
  uuid?: () => string;
  timers?: Timers;
  maxActiveJobs?: number;
  partConcurrency?: number;
  maxAutoAttempts?: number;
  /** Duration of a bare-Blob video segment (browser: a <video> metadata probe). */
  probeDuration?: (blob: Blob) => Promise<number>;
  /** Called after every write to storage (cross-tab mirroring). */
  onPersist?: () => void;
  /** Snapshot notifications are coalesced to at most one per this many ms (0 = synchronous). */
  notifyThrottleMs?: number;
}

type Live = { processFrac: number; inflight: Map<string, number> };

export class StatusQueue {
  private readonly storage: QueueStorage;
  private readonly transport: StatusTransport;
  private readonly persistent: boolean;
  private readonly now: () => number;
  private readonly random: () => number;
  private readonly uuid: () => string;
  private readonly timers: Timers;
  private readonly maxActiveJobs: number;
  private readonly partConcurrency: number;
  private readonly maxAutoAttempts: number;
  private readonly probeDuration?: (blob: Blob) => Promise<number>;
  private readonly onPersist?: () => void;
  private readonly notifyThrottleMs: number;

  private jobs = new Map<string, StoredJob>();
  private blobs = new Map<string, Blob>();
  private closures = new Map<string, (ctx: ProcessorContext) => Promise<ProcessorResult>>();
  private processors = new Map<string, StatusProcessor>();
  private controllers = new Map<string, AbortController>();
  private runners = new Map<string, Promise<void>>();
  private publisher: Promise<void> | null = null;
  /** pump() ran while the publisher was busy: look again when it finishes. */
  private publishAgain = false;
  private publishingJob: string | null = null;
  private live = new Map<string, Live>();
  private finished = new Map<string, JobView>();
  /** Uploads whose parts were checked with the server in this session. */
  private verified = new Set<string>();
  private unpersisted = new Set<string>();
  private failedNotified = new Set<string>();
  /** Finished or cancelled here: a stale storage read must never bring them back. */
  private retired = new Set<string>();

  private ownerId: string | null = null;
  private started = false;
  private active = true;
  private online = true;
  private lastOrder = 0;

  private wakeTimer: unknown = null;
  private wakeAt = 0;
  private notifyTimer: unknown = null;
  private dirty = true;
  private snapshot: QueueSnapshot = { jobs: [], online: true, persistent: false };
  private listeners = new Set<() => void>();
  private eventListeners = new Set<(e: QueueEvent) => void>();

  constructor(opts: QueueOptions) {
    this.storage = opts.storage;
    this.transport = opts.transport;
    this.persistent = opts.persistent ?? true;
    this.now = opts.now ?? (() => Date.now());
    this.random = opts.random ?? Math.random;
    this.uuid = opts.uuid ?? defaultUuid;
    this.timers = opts.timers ?? { set: (fn, ms) => setTimeout(fn, ms), clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>) };
    this.maxActiveJobs = Math.max(1, opts.maxActiveJobs ?? 2);
    this.partConcurrency = Math.max(1, opts.partConcurrency ?? 3);
    this.maxAutoAttempts = Math.max(1, opts.maxAutoAttempts ?? MAX_AUTO_ATTEMPTS);
    this.probeDuration = opts.probeDuration;
    this.onPersist = opts.onPersist;
    this.notifyThrottleMs = opts.notifyThrottleMs ?? 0;
  }

  // -------------------------------------------------------------------------------- lifecycle

  /**
   * Loads this owner's jobs from storage and (when `active`) resumes them. A job whose
   * closure processing was interrupted by the reload is marked failed (cancel only).
   */
  async start(ownerId: string | null, active = true): Promise<void> {
    this.ownerId = ownerId;
    this.active = active;
    this.started = true;
    // Jobs enqueued before the owner was known belong to this owner.
    for (const job of this.jobs.values()) if (job.ownerId === null) { job.ownerId = ownerId; void this.persist(job); }
    await this.reload();
    this.pump();
  }

  /** Stops all work (aborts in-flight requests without marking anything failed). */
  stop(): void {
    this.started = false;
    for (const c of this.controllers.values()) c.abort();
    this.controllers.clear();
    if (this.wakeTimer !== null) this.timers.clear(this.wakeTimer);
    this.wakeTimer = null;
    this.wakeAt = 0;
  }

  /** Leader (runs the queue) or follower (mirrors storage). */
  async setActive(active: boolean): Promise<void> {
    if (active === this.active) return;
    if (!active) {
      for (const c of this.controllers.values()) c.abort();
      this.controllers.clear();
    }
    this.active = active;
    if (active) {
      // Becoming the leader: storage is the truth, except jobs this tab couldn't store.
      for (const id of [...this.jobs.keys()]) if (!this.unpersisted.has(id)) this.jobs.delete(id);
    }
    await this.reload();
    this.pump();
  }

  /**
   * Re-reads storage. Active: adopts jobs written by another tab. Follower: mirrors storage
   * (the leader's writes) entirely.
   */
  async reload(): Promise<void> {
    let stored: StoredJob[];
    try { stored = await this.storage.loadJobs(); } catch { stored = []; }
    const mine = stored.filter((j) => j.ownerId === this.ownerId);
    if (!this.active) {
      const ids = new Set(mine.map((j) => j.id));
      for (const id of [...this.jobs.keys()]) if (!ids.has(id) && !this.unpersisted.has(id)) this.jobs.delete(id);
    }
    for (const job of mine) {
      if (this.retired.has(job.id)) continue;
      this.lastOrder = Math.max(this.lastOrder, job.order);
      if (this.active && this.jobs.has(job.id)) continue;
      if (this.active && job.process?.mode === "closure" && !this.closures.has(job.id) && !job.failed) {
        job.failed = true;
        job.fatal = true;
        job.error = "Processing was interrupted. Edit it again.";
        await this.writeJob(job);
      }
      // A fresh start (reload / new leader) tries again at once rather than honour an old back-off.
      if (this.active) job.nextAttemptAt = 0;
      this.jobs.set(job.id, job);
      await this.loadPreviewBlobs(job);
    }
    this.changed();
  }

  // -------------------------------------------------------------------------------- public API

  /** Adds a job; resolves once it is queued (memory + storage, best effort). Returns the job id. */
  async enqueue(input: StatusJobInput): Promise<string> {
    if (!input.items.length) throw new Error("A status job needs at least one item.");
    const id = this.uuid();
    const job: StoredJob = {
      id,
      groupId: this.uuid(),
      ownerId: this.ownerId,
      createdAt: new Date(this.now()).toISOString(),
      order: (this.lastOrder = Math.max(this.now(), this.lastOrder + 1)),
      posts: [],
      uploads: [],
      process: null,
      preview: { kind: "TEXT", text: "", background: null, font: 0 },
      attempts: 0,
      nextAttemptAt: 0,
      failed: false,
      error: null,
      fatal: false,
      hasProcess: false,
      audience: input.audience ? { ...input.audience, ...(input.audience.principalIds ? { principalIds: [...input.audience.principalIds] } : {}) } : null,
      allowSharing: typeof input.allowSharing === "boolean" ? input.allowSharing : null,
      audienceLabel: input.audienceLabel ?? null,
    };
    const toStore: [string, Blob][] = [];
    const keep = (key: string, blob: Blob) => { this.blobs.set(key, blob); toStore.push([key, blob]); return key; };
    let n = 0;
    const addUpload = (item: number, blob: Blob, kind: "IMAGE" | "VIDEO" | "AUDIO", durationMs: number | null): StoredUpload => {
      const key = `u${n++}`;
      const u: StoredUpload = {
        key, item, blobKey: keep(`${id}/${key}`, blob), kind, contentType: contentTypeOf(blob, kind), sizeBytes: blob.size,
        durationMs: durationMs === null ? null : Math.round(durationMs), uploadId: null, partSize: null, partCount: null, partsDone: [], mediaId: null,
      };
      job.uploads.push(u);
      return u;
    };
    const addPost = (item: number, uploadKey: string | null, body: StoredPost["body"], placeholder = false, musicUploadKey: string | null = null) => {
      job.posts.push({ sequence: job.posts.length, item, idempotencyKey: this.uuid(), uploadKey, body, postId: null, uncertain: false, ...(placeholder ? { placeholder } : {}), ...(musicUploadKey ? { musicUploadKey } : {}) });
    };
    // Music on a text / photo status: its own AUDIO upload, referenced as musicMediaId.
    const addMusic = (item: number, music: { blob: Blob; durationMs: number; title?: string } | undefined) =>
      music ? { key: addUpload(item, music.blob, "AUDIO", music.durationMs).key, title: clean(music.title)?.slice(0, 120) } : null;
    const clean = (s: string | undefined) => (s && s.trim() ? s.trim() : undefined);
    const mentionsOf = (m: string[] | undefined) => (m && m.length ? [...new Set(m)] : undefined);

    // The card previews the job's last item.
    for (const [i, item] of input.items.entries()) {
      const mentions = mentionsOf(item.mentions);
      switch (item.kind) {
        case "TEXT": {
          const music = addMusic(i, item.music);
          const style = { textColor: item.textColor, textAlign: item.textAlign, textSize: item.textSize, backgroundGradient: item.backgroundGradient };
          addPost(i, null, { kind: "TEXT", text: item.text, background: item.backgroundGradient ? undefined : item.background, font: item.font, ...style, musicTitle: music?.title, mentions }, false, music?.key ?? null);
          job.preview = { kind: "TEXT", text: item.text, background: item.background ?? null, font: item.font ?? 0 };
          break;
        }
        case "IMAGE": {
          const u = addUpload(i, item.blob, "IMAGE", null);
          const music = addMusic(i, item.music);
          addPost(i, u.key, { kind: "IMAGE", caption: clean(item.caption), musicTitle: music?.title, mentions }, false, music?.key ?? null);
          job.preview = { kind: "IMAGE", blobKey: item.preview ? keep(`${id}/preview`, item.preview) : u.blobKey, ...(music ? { music: true } : {}) };
          break;
        }
        case "AUDIO": {
          const u = addUpload(i, item.blob, "AUDIO", item.durationMs);
          const title = clean(item.musicTitle)?.slice(0, 120);
          addPost(i, u.key, {
            kind: "AUDIO", caption: clean(item.caption), mentions, musicTitle: title,
            ...(item.backgroundGradient ? { backgroundGradient: item.backgroundGradient } : { background: item.background }),
          });
          job.preview = { kind: "AUDIO", blobKey: null, background: item.background ?? null, ...(title ? { music: true } : {}) };
          break;
        }
        case "VIDEO": {
          const caption = clean(item.caption);
          const firstUpload = job.uploads.length;
          if ("segments" in item) {
            if (!item.segments.length) throw new Error("A video needs at least one segment.");
            for (const [s, seg] of item.segments.entries()) {
              const blob = seg instanceof Blob ? seg : seg.blob;
              let ms = seg instanceof Blob ? null : seg.durationMs;
              if (ms === null) {
                if (!this.probeDuration) throw new Error("A bare video segment needs its duration.");
                ms = await this.probeDuration(blob);
              }
              const u = addUpload(i, blob, "VIDEO", ms);
              addPost(i, u.key, { kind: "VIDEO", startMs: 0, endMs: Math.round(ms), caption: s === 0 ? caption : undefined, mentions: s === 0 ? mentions : undefined });
            }
          } else if ("source" in item) {
            if (!item.ranges.length) throw new Error("A video needs at least one range.");
            const u = addUpload(i, item.source, "VIDEO", item.durationMs);
            for (const [r, range] of item.ranges.entries()) {
              addPost(i, u.key, { kind: "VIDEO", startMs: Math.round(range.startMs), endMs: Math.round(range.endMs), caption: r === 0 ? caption : undefined, mentions: r === 0 ? mentions : undefined });
            }
          } else {
            if (job.process) throw new Error("At most one deferred video per status job.");
            if ("process" in item) {
              const blobKeys = item.process.blobs && Object.keys(item.process.blobs).length
                ? Object.fromEntries(Object.entries(item.process.blobs).map(([name, b]) => [name, keep(`${id}/input-${name}`, b)]))
                : null;
              job.process = { itemIndex: i, mode: "named", processor: item.process.processor, inputBlobKey: keep(`${id}/input`, item.process.input), params: item.process.params ?? null, blobKeys, caption: caption ?? null, mentions: mentions ?? null };
            } else {
              job.process = { itemIndex: i, mode: "closure", processor: null, inputBlobKey: null, params: null, caption: caption ?? null, mentions: mentions ?? null };
              this.closures.set(id, item.prepare);
            }
            addPost(i, null, { kind: "VIDEO" }, true);
          }
          const video = job.process?.itemIndex === i ? job.process.inputBlobKey : job.uploads[firstUpload]?.blobKey ?? null;
          job.preview = { kind: "VIDEO", blobKey: item.thumbnail ? keep(`${id}/preview`, item.thumbnail) : null, videoBlobKey: video };
          break;
        }
      }
    }
    job.hasProcess = job.process !== null;
    this.jobs.set(id, job);
    this.unpersisted.add(id);
    this.changed();
    try {
      for (const [key, blob] of toStore) await this.storage.putBlob(key, blob);
      await this.storage.putJob(structuredCloneJob(job));
      this.unpersisted.delete(id);
      this.onPersist?.();
    } catch {
      // Kept in memory: it still publishes in this session, it just won't survive a reload.
    }
    this.pump();
    return id;
  }

  /** Retries a failed job (from where it stopped: uploaded parts and published items are kept). */
  retry(id: string): void {
    const job = this.jobs.get(id);
    if (!job || !job.failed || job.fatal) return;
    job.failed = false;
    job.error = null;
    job.attempts = 0;
    job.nextAttemptAt = 0;
    this.failedNotified.delete(id);
    void this.persist(job);
    this.changed();
    this.pump();
  }

  /**
   * Cancels a job: in-flight work is aborted, the server-side upload discarded (when the
   * transport can), local data erased. Statuses of the job already published stay.
   */
  async cancel(id: string): Promise<void> {
    const job = this.jobs.get(id);
    if (!job) return;
    this.controllers.get(id)?.abort();
    this.controllers.delete(id);
    this.jobs.delete(id);
    this.retired.add(id);
    this.closures.delete(id);
    this.live.delete(id);
    this.unpersisted.delete(id);
    this.failedNotified.delete(id);
    this.dropBlobs(id);
    this.changed();
    for (const u of job.uploads) if (u.uploadId && !u.mediaId) void this.transport.cancelUpload(u.uploadId).catch(() => {});
    try {
      await this.storage.deleteJob(id);
      await this.storage.deleteBlobs(id);
      this.onPersist?.();
    } catch { /* storage unavailable */ }
    this.pump();
  }

  /** A published job leaves the snapshot (call once the feed shows its statuses). */
  acknowledge(id: string): void {
    if (this.finished.delete(id)) this.changed();
  }

  setOnline(online: boolean): void {
    if (online === this.online) return;
    this.online = online;
    if (online) {
      // Back online: whatever was waiting on a back-off is due now.
      for (const job of this.jobs.values()) if (!job.failed) job.nextAttemptAt = 0;
    }
    this.changed();
    this.pump();
  }

  /** Re-evaluate now (tab visible again, timers throttled while hidden…). */
  kick(): void { this.pump(); }

  registerProcessor(name: string, fn: StatusProcessor): void {
    this.processors.set(name, fn);
    for (const job of this.jobs.values()) {
      if (job.failed && !job.fatal && job.process?.mode === "named" && job.process.processor === name) this.retry(job.id);
    }
  }

  /** Erases every job of every owner (sign-out). */
  async clearAll(): Promise<void> {
    this.stop();
    this.jobs.clear();
    this.finished.clear();
    this.blobs.clear();
    this.closures.clear();
    this.live.clear();
    this.changed();
    try { await this.storage.clear(); } catch { /* nothing stored */ }
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  onEvent(listener: (e: QueueEvent) => void): () => void {
    this.eventListeners.add(listener);
    return () => { this.eventListeners.delete(listener); };
  }

  getSnapshot = (): QueueSnapshot => {
    if (this.dirty) {
      this.dirty = false;
      const views = [...this.jobs.values()].sort((a, b) => a.order - b.order).map((j) => this.view(j));
      const done = [...this.finished.values()];
      this.snapshot = { jobs: [...views, ...done].sort((a, b) => a.createdAt.localeCompare(b.createdAt)), online: this.online, persistent: this.persistent };
    }
    return this.snapshot;
  };

  /** Resolves when nothing is running (tests). */
  async settled(): Promise<void> {
    for (;;) {
      const pending = [...this.runners.values(), ...(this.publisher ? [this.publisher] : [])];
      if (!pending.length) return;
      await Promise.allSettled(pending);
      await Promise.resolve();
    }
  }

  // -------------------------------------------------------------------------------- scheduler

  private pump(): void {
    if (!this.started || !this.active) return;
    if (this.online) {
      const now = this.now();
      const due = [...this.jobs.values()]
        .filter((j) => !j.failed && !this.runners.has(j.id) && j.nextAttemptAt <= now && this.needsWork(j))
        .sort((a, b) => a.order - b.order);
      for (const job of due) {
        if (this.runners.size >= this.maxActiveJobs) break;
        this.startRunner(job);
      }
      if (!this.publisher) {
        this.publishAgain = false;
        const p = this.publishLoop().finally(() => {
          if (this.publisher === p) this.publisher = null;
          if (this.publishAgain) this.pump();
        });
        this.publisher = p;
      } else {
        this.publishAgain = true;
      }
    }
    this.scheduleWake();
  }

  private scheduleWake(): void {
    const now = this.now();
    let next = Infinity;
    if (this.online) for (const j of this.jobs.values()) if (!j.failed && j.nextAttemptAt > now) next = Math.min(next, j.nextAttemptAt);
    if (next === Infinity) return;
    if (this.wakeTimer !== null && this.wakeAt <= next) return;
    if (this.wakeTimer !== null) this.timers.clear(this.wakeTimer);
    this.wakeAt = next;
    this.wakeTimer = this.timers.set(() => { this.wakeTimer = null; this.wakeAt = 0; this.changed(); this.pump(); }, next - now);
  }

  private needsWork(job: StoredJob): boolean {
    return job.process !== null || job.uploads.some((u) => !u.mediaId);
  }

  private controller(id: string): AbortController {
    let c = this.controllers.get(id);
    if (!c || c.signal.aborted) { c = new AbortController(); this.controllers.set(id, c); }
    return c;
  }

  private startRunner(job: StoredJob): void {
    const signal = this.controller(job.id).signal;
    const run = (async () => {
      try {
        if (job.process) await this.runProcess(job, signal);
        for (const u of job.uploads) {
          if (u.mediaId) continue;
          await this.runUpload(job, u, signal);
          this.pump();
        }
      } catch (e) {
        this.handleFailure(job, e, signal);
      }
    })();
    const tracked = run.finally(() => {
      this.runners.delete(job.id);
      this.changed();
      this.pump();
    });
    this.runners.set(job.id, tracked);
    this.changed();
  }

  // -------------------------------------------------------------------------------- processing

  private async runProcess(job: StoredJob, signal: AbortSignal): Promise<void> {
    const p = job.process!;
    const live = this.liveOf(job.id);
    const blobs: Record<string, Blob> = {};
    for (const [name, key] of Object.entries(p.blobKeys ?? {})) {
      const b = await this.blob(key);
      if (!b) throw new FatalError("A file this status needs is no longer on this device.");
      blobs[name] = b;
    }
    const ctx: ProcessorContext = { signal, blobs, onProgress: (f) => { live.processFrac = Math.max(0, Math.min(1, f)); this.changed(); } };
    let result: ProcessorResult;
    if (p.mode === "named") {
      const fn = p.processor ? this.processors.get(p.processor) : undefined;
      if (!fn) throw new ProcessorMissing();
      const input = p.inputBlobKey ? await this.blob(p.inputBlobKey) : null;
      if (!input) throw new FatalError("The edited video is no longer on this device.");
      result = await fn(input, p.params, ctx);
    } else {
      const fn = this.closures.get(job.id);
      if (!fn) throw new FatalError("Processing was interrupted. Edit it again.");
      result = await fn(ctx);
    }
    if (signal.aborted || !this.jobs.has(job.id)) return;
    const pieces = "segments" in result ? result.segments : [{ blob: result.source, durationMs: result.durationMs }];
    const ranges = "segments" in result ? null : result.ranges;
    if (!pieces.length || (ranges && !ranges.length)) throw new FatalError("The video produced nothing to post.");

    // Persist the outputs before the job record points at them (a fallback to the edit's own
    // input reuses its stored copy).
    const fresh: StoredUpload[] = [];
    let keepInput = false;
    for (const [s, seg] of pieces.entries()) {
      const key = `p${s}`;
      let blobKey = `${job.id}/${key}`;
      if (p.inputBlobKey && seg.blob === (await this.blob(p.inputBlobKey))) {
        blobKey = p.inputBlobKey;
        keepInput = true;
      } else {
        this.blobs.set(blobKey, seg.blob);
        await this.storage.putBlob(blobKey, seg.blob).catch(() => {});
      }
      fresh.push({ key, item: p.itemIndex, blobKey, kind: "VIDEO", contentType: contentTypeOf(seg.blob, "VIDEO"), sizeBytes: seg.blob.size, durationMs: Math.round(seg.durationMs), uploadId: null, partSize: null, partCount: null, partsDone: [], mediaId: null });
    }
    if (result.thumbnail && job.preview.kind !== "TEXT" && !job.preview.blobKey) {
      const key = `${job.id}/preview`;
      this.blobs.set(key, result.thumbnail);
      await this.storage.putBlob(key, result.thumbnail).catch(() => {});
      job.preview = { ...job.preview, blobKey: key };
    }
    const at = job.uploads.findIndex((u) => u.item > p.itemIndex);
    job.uploads.splice(at === -1 ? job.uploads.length : at, 0, ...fresh);
    const placeholder = job.posts.findIndex((x) => x.item === p.itemIndex && x.placeholder);
    const bodies = ranges
      ? ranges.map((r) => ({ uploadKey: fresh[0].key, startMs: Math.round(r.startMs), endMs: Math.round(r.endMs) }))
      : fresh.map((u) => ({ uploadKey: u.key, startMs: 0, endMs: u.durationMs ?? 0 }));
    const posts: StoredPost[] = bodies.map((b, s) => ({
      sequence: 0, item: p.itemIndex, idempotencyKey: this.uuid(), uploadKey: b.uploadKey,
      body: { kind: "VIDEO", startMs: b.startMs, endMs: b.endMs, caption: s === 0 && p.caption ? p.caption : undefined, mentions: s === 0 && p.mentions?.length ? p.mentions : undefined },
      postId: null, uncertain: false,
    }));
    // The placeholder's key is kept for the first piece: one key per item for its whole life.
    if (placeholder !== -1) posts[0].idempotencyKey = job.posts[placeholder].idempotencyKey;
    job.posts.splice(placeholder === -1 ? job.posts.length : placeholder, placeholder === -1 ? 0 : 1, ...posts);
    job.posts.forEach((x, i) => { x.sequence = i; });
    const inputKey = p.inputBlobKey;
    if (job.preview.kind !== "TEXT" && job.preview.kind === "VIDEO" && (job.preview.videoBlobKey === inputKey || !job.preview.videoBlobKey)) {
      job.preview = { ...job.preview, videoBlobKey: fresh[0].blobKey };
    }
    job.process = null;
    this.closures.delete(job.id);
    live.processFrac = 1;
    await this.persist(job);
    if (inputKey && !keepInput) {
      // The edit's input is no longer needed once its outputs are stored.
      this.blobs.delete(inputKey);
      await this.storage.deleteBlob(inputKey).catch(() => {});
    }
    for (const key of Object.values(p.blobKeys ?? {})) {
      this.blobs.delete(key);
      await this.storage.deleteBlob(key).catch(() => {});
    }
  }

  // -------------------------------------------------------------------------------- uploading

  private async runUpload(job: StoredJob, u: StoredUpload, signal: AbortSignal): Promise<void> {
    const blob = await this.blob(u.blobKey);
    if (!blob) throw new FatalError("The media is no longer on this device.");
    const live = this.liveOf(job.id);
    try {
      if (!u.uploadId) {
        const started = await this.transport.startUpload({ kind: u.kind, contentType: u.contentType, sizeBytes: u.sizeBytes, durationMs: u.durationMs }, signal);
        if (signal.aborted) return;
        u.uploadId = started.uploadId;
        u.partSize = started.partSize;
        u.partCount = started.partCount;
        u.partsDone = [];
        this.verified.add(u.key + job.id);
        await this.persist(job);
      } else if (!this.verified.has(u.key + job.id)) {
        // Resume after a reload / handover: the server's view of the parts wins.
        const stored = this.transport.capabilities.listParts ? await this.transport.storedParts(u.uploadId, signal) : null;
        if (stored) u.partsDone = [...new Set(stored)].filter((i) => i >= 0 && i < (u.partCount ?? 0)).sort((a, b) => a - b);
        this.verified.add(u.key + job.id);
        await this.persist(job);
      }
      const uploadId = u.uploadId!;
      const missing: number[] = [];
      for (let i = 0; i < (u.partCount ?? 0); i++) if (!u.partsDone.includes(i)) missing.push(i);
      let cursor = 0;
      let firstError: unknown = null;
      const worker = async () => {
        while (firstError === null && !signal.aborted && cursor < missing.length) {
          const index = missing[cursor++];
          const start = index * (u.partSize ?? 0);
          const piece = blob.slice(start, Math.min(u.sizeBytes, start + (u.partSize ?? 0)));
          const tag = `${u.key}:${index}`;
          try {
            await this.transport.uploadPart(uploadId, index, piece, signal, (sent) => { live.inflight.set(tag, sent); this.changed(); });
          } catch (e) {
            if (firstError === null) firstError = e;
            return;
          } finally {
            live.inflight.delete(tag);
          }
          if (!this.jobs.has(job.id)) return;
          u.partsDone.push(index);
          job.attempts = 0;
          await this.persist(job);
        }
      };
      await Promise.all(Array.from({ length: Math.min(this.partConcurrency, missing.length) }, worker));
      if (firstError !== null) throw firstError;
      if (signal.aborted) return;
      const done = await this.transport.completeUpload(uploadId, signal);
      if (signal.aborted) return;
      u.mediaId = done.mediaId;
      job.attempts = 0;
      await this.persist(job);
    } catch (e) {
      if (isTransportError(e) && e.kind === "gone" && !signal.aborted) {
        // The server no longer knows this upload: restart it (this one only), parts and all.
        u.uploadId = null;
        u.partSize = null;
        u.partCount = null;
        u.partsDone = [];
        await this.persist(job);
      }
      throw e;
    }
  }

  // -------------------------------------------------------------------------------- publishing

  private async publishLoop(): Promise<void> {
    while (this.started && this.active && this.online) {
      const next = this.nextPost();
      if (!next) return;
      const { job, post } = next;
      const signal = this.controller(job.id).signal;
      this.publishingJob = job.id;
      this.changed();
      try {
        await this.publishPost(job, post, signal);
      } catch (e) {
        this.handleFailure(job, e, signal);
      } finally {
        this.publishingJob = null;
        this.changed();
      }
      if (signal.aborted && this.jobs.has(job.id)) return;
    }
  }

  /** The next status to create, in strict order; null when the head of the line isn't ready. */
  private nextPost(): { job: StoredJob; post: StoredPost } | null {
    const now = this.now();
    for (const job of [...this.jobs.values()].sort((a, b) => a.order - b.order)) {
      if (job.failed) continue; // a failed job steps aside
      const post = job.posts.find((x) => !x.postId);
      if (!post) { this.finish(job); continue; }
      if (post.placeholder || job.nextAttemptAt > now) return null;
      if (post.uploadKey && !job.uploads.find((u) => u.key === post.uploadKey)?.mediaId) return null;
      if (post.musicUploadKey && !job.uploads.find((u) => u.key === post.musicUploadKey)?.mediaId) return null;
      return { job, post };
    }
    return null;
  }

  private async publishPost(job: StoredJob, post: StoredPost, signal: AbortSignal): Promise<void> {
    const upload = post.uploadKey ? job.uploads.find((u) => u.key === post.uploadKey) : null;
    // The job's audience and sharing go with EVERY create (each split segment, each item).
    const req = { ...post.body, groupId: job.groupId, sequence: post.sequence } as Record<string, unknown>;
    if (job.audience) req.audience = job.audience;
    if (typeof job.allowSharing === "boolean") req.allowSharing = job.allowSharing;
    if (upload) req.mediaId = upload.mediaId;
    const music = post.musicUploadKey ? job.uploads.find((u) => u.key === post.musicUploadKey) : null;
    if (music) req.musicMediaId = music.mediaId;
    for (const k of Object.keys(req)) if (req[k] === undefined) delete req[k];
    const request = req as unknown as StatusCreateRequest;

    if (post.uncertain && !this.transport.capabilities.idempotentCreate && this.transport.findCreatedStatus) {
      // Degraded mode (no server idempotency): did the lost attempt actually create it?
      const since = new Date(Date.parse(job.createdAt) - FIND_SKEW_MS).toISOString();
      const candidates = await this.transport.findCreatedStatus(request, since, signal);
      const claimed = this.claimedPostIds();
      const hit = candidates?.find((c) => !claimed.has(c.postId));
      if (hit) {
        post.postId = hit.postId;
        post.uncertain = false;
        await this.persist(job);
        this.afterPublished(job);
        return;
      }
    }
    post.uncertain = true; // until the answer arrives, the outcome is unknown
    await this.persist(job);
    let created: { postId: string };
    try {
      created = await this.transport.createStatus(request, post.idempotencyKey, signal);
    } catch (e) {
      if (isTransportError(e) && (e.kind === "rejected" || e.kind === "auth" || e.kind === "rate")) post.uncertain = false; // definitely not created
      await this.persist(job);
      throw e;
    }
    if (!this.jobs.has(job.id)) return;
    post.postId = created.postId;
    post.uncertain = false;
    job.attempts = 0;
    await this.persist(job);
    this.afterPublished(job);
  }

  private afterPublished(job: StoredJob): void {
    if (job.posts.every((x) => x.postId)) this.finish(job);
    this.changed();
  }

  private claimedPostIds(): Set<string> {
    const out = new Set<string>();
    for (const j of this.jobs.values()) for (const p of j.posts) if (p.postId) out.add(p.postId);
    return out;
  }

  private finish(job: StoredJob): void {
    if (!this.jobs.has(job.id)) return;
    const view = { ...this.view(job), state: "done" as JobState, progress: 1 };
    this.jobs.delete(job.id);
    this.retired.add(job.id);
    this.live.delete(job.id);
    this.controllers.delete(job.id);
    this.finished.set(job.id, view);
    this.dropBlobs(job.id);
    void (async () => {
      try { await this.storage.deleteJob(job.id); await this.storage.deleteBlobs(job.id); this.onPersist?.(); } catch { /* best effort */ }
    })();
    this.changed();
    this.emitEvent({ type: "published", job: view });
  }

  // -------------------------------------------------------------------------------- failures

  private handleFailure(job: StoredJob, e: unknown, signal: AbortSignal): void {
    if (signal.aborted || !this.jobs.has(job.id) || (isTransportError(e) && e.kind === "aborted")) return;
    if (e instanceof ProcessorMissing) {
      this.fail(job, "Still preparing this video. It will continue automatically.", false);
      return;
    }
    if (e instanceof FatalError) { this.fail(job, e.message, true); return; }
    if (!isTransportError(e)) { this.fail(job, e instanceof Error && e.message ? e.message : "Something went wrong.", false); return; }
    if (!this.online) {
      // Offline: nothing is counted; `online` resumes it.
      job.nextAttemptAt = 0;
      void this.persist(job);
      this.changed();
      return;
    }
    if (e.kind === "rejected") { this.fail(job, e.message || "The server refused this status.", false); return; }
    if (e.kind === "auth") { this.fail(job, "Sign in again to finish posting.", false); return; }
    job.attempts += 1;
    if (job.attempts >= this.maxAutoAttempts) { this.fail(job, "Failed to upload", false); return; }
    job.nextAttemptAt = this.now() + Math.max(e.kind === "rate" ? 5000 : 0, backoffMs(job.attempts, this.random));
    void this.persist(job);
    this.changed();
  }

  private fail(job: StoredJob, message: string, fatal: boolean): void {
    job.failed = true;
    job.fatal = fatal;
    job.error = message;
    void this.persist(job);
    this.changed();
    if (!this.failedNotified.has(job.id)) {
      this.failedNotified.add(job.id);
      this.emitEvent({ type: "failed", job: this.view(job) });
    }
  }

  // -------------------------------------------------------------------------------- views

  private view(job: StoredJob): JobView {
    const published = job.posts.filter((p) => p.postId).length;
    return {
      id: job.id,
      groupId: job.groupId,
      createdAt: job.createdAt,
      state: this.stateOf(job),
      progress: this.progressOf(job),
      processing: job.hasProcess ? (job.process ? this.live.get(job.id)?.processFrac ?? 0 : 1) : null,
      upload: job.process ? null : this.uploadBytes(job),
      error: job.error,
      fatal: job.fatal,
      preview: this.previewView(job),
      postCount: job.posts.length,
      publishedCount: published,
      audienceLabel: job.audienceLabel ?? null,
    };
  }

  private stateOf(job: StoredJob): JobState {
    if (job.failed) return "failed";
    if (this.runners.has(job.id)) return job.process ? "processing" : "uploading";
    if (this.publishingJob === job.id) return "publishing";
    if (!this.online || job.nextAttemptAt > this.now()) return "waiting";
    return "queued";
  }

  private progressOf(job: StoredJob): number {
    const live = this.live.get(job.id);
    const wProc = job.hasProcess ? PROCESS_WEIGHT : 0;
    const procFrac = job.process ? live?.processFrac ?? 0 : 1;
    const hasUploads = job.uploads.length > 0 || job.process !== null;
    const wPub = hasUploads ? PUBLISH_WEIGHT : 1 - wProc;
    const wUp = 1 - wProc - wPub;
    const { sentBytes: sent, totalBytes: total } = this.uploadBytes(job) ?? { sentBytes: 0, totalBytes: 0 };
    const upFrac = job.process ? 0 : total > 0 ? Math.min(1, sent / total) : 1;
    const pubFrac = job.posts.length ? job.posts.filter((p) => p.postId).length / job.posts.length : 0;
    return Math.max(0, Math.min(1, wProc * procFrac + wUp * upFrac + wPub * pubFrac));
  }

  /** Bytes of the job's uploads acknowledged or in flight, and in all; null with nothing to upload. */
  private uploadBytes(job: StoredJob): { sentBytes: number; totalBytes: number } | null {
    if (!job.uploads.length) return null;
    const live = this.live.get(job.id);
    let total = 0;
    let sent = 0;
    for (const u of job.uploads) {
      total += u.sizeBytes;
      if (u.mediaId) { sent += u.sizeBytes; continue; }
      for (const i of u.partsDone) sent += partBytes(u, i);
      if (live) for (const [tag, bytes] of live.inflight) if (tag.startsWith(`${u.key}:`)) sent += bytes;
    }
    return { sentBytes: Math.min(sent, total), totalBytes: total };
  }

  private previewView(job: StoredJob): JobPreview {
    const p = job.preview;
    if (p.kind === "TEXT") return p;
    return {
      kind: p.kind,
      blob: p.blobKey ? this.blobs.get(p.blobKey) ?? null : null,
      video: p.videoBlobKey ? this.blobs.get(p.videoBlobKey) ?? null : null,
      background: p.background ?? null,
      ...(p.music ? { music: true } : {}),
    };
  }

  // -------------------------------------------------------------------------------- storage

  private async blob(key: string): Promise<Blob | null> {
    const hit = this.blobs.get(key);
    if (hit) return hit;
    try {
      const b = await this.storage.getBlob(key);
      return b;
    } catch {
      return null;
    }
  }

  private async loadPreviewBlobs(job: StoredJob): Promise<void> {
    const p = job.preview;
    if (p.kind === "TEXT") return;
    for (const key of [p.blobKey, p.videoBlobKey]) {
      if (!key || this.blobs.has(key)) continue;
      const b = await this.storage.getBlob(key).catch(() => null);
      if (b) this.blobs.set(key, b);
    }
  }

  private dropBlobs(jobId: string): void {
    for (const k of [...this.blobs.keys()]) if (k.startsWith(`${jobId}/`)) this.blobs.delete(k);
  }

  private async persist(job: StoredJob): Promise<void> {
    if (this.jobs.get(job.id) !== job || !this.active) return;
    await this.writeJob(job);
  }

  private async writeJob(job: StoredJob): Promise<void> {
    try {
      await this.storage.putJob(structuredCloneJob(job));
      this.unpersisted.delete(job.id);
      this.onPersist?.();
    } catch { /* memory only */ }
  }

  private liveOf(id: string): Live {
    let l = this.live.get(id);
    if (!l) { l = { processFrac: 0, inflight: new Map() }; this.live.set(id, l); }
    return l;
  }

  // -------------------------------------------------------------------------------- notify

  private changed(): void {
    this.dirty = true;
    if (this.notifyThrottleMs <= 0) { this.notify(); return; }
    if (this.notifyTimer !== null) return;
    this.notifyTimer = this.timers.set(() => { this.notifyTimer = null; this.notify(); }, this.notifyThrottleMs);
  }

  private notify(): void {
    for (const l of [...this.listeners]) l();
  }

  private emitEvent(e: QueueEvent): void {
    for (const l of [...this.eventListeners]) { try { l(e); } catch { /* listener bug */ } }
  }
}

class FatalError extends Error {}
class ProcessorMissing extends Error {}


/** A plain copy for storage (never shares arrays with the live record). */
function structuredCloneJob(job: StoredJob): StoredJob {
  return JSON.parse(JSON.stringify(job)) as StoredJob;
}
