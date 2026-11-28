/**
 * The browser's status queue: one per page, independent of React and of routes, so publishing
 * keeps going while the user moves between chats, calls, meetings and settings.
 *
 * * Storage: IndexedDB (jobs + media Blobs) — pending statuses survive reloads and restarts and
 *   resume automatically on the next start. Without IndexedDB it falls back to memory.
 * * One tab works the queue (a Web Lock elects it); other tabs mirror its progress from
 *   storage and hand their new jobs / retry / cancel to it over a BroadcastChannel. When that
 *   tab closes, another takes over and resumes from storage. (Two tabs uploading the same
 *   job would duplicate work, and — with today's transport — possibly statuses.)
 * * Network: paused on `offline`, resumed on `online`, re-evaluated when the tab becomes
 *   visible again (background timers are throttled by browsers).
 *
 * Swap the transport (Messaging-backed upload API) in `createTransport` below.
 */
import { durationOf } from "@/services/v2/social.service";
import { StatusQueue } from "./queue";
import { createSocialTransport } from "./social-transport";
import { IndexedDbQueueStorage, MemoryQueueStorage } from "./storage";
import type { StatusJobInput, StatusProcessor, StatusTransport } from "./types";

let transportOverride: StatusTransport | null = null;

/** THE swap point: the transport the app uses. */
function createTransport(): StatusTransport {
  return transportOverride ?? createSocialTransport();
}

/** Uses another transport (call before the queue is first used, e.g. in an integration shim). */
export function configureStatusQueue(opts: { transport: StatusTransport }): void {
  if (queue) throw new Error("The status queue is already running.");
  transportOverride = opts.transport;
}

const CHANNEL = "cfmeet-status-queue";
type Message = { type: "changed" } | { type: "retry"; id: string } | { type: "cancel"; id: string };

let queue: StatusQueue | null = null;
let channel: BroadcastChannel | null = null;
let leader = false;
let ownerKey: string | null = null;
let teardown: (() => void) | null = null;
let changedTimer: ReturnType<typeof setTimeout> | null = null;

function broadcast(m: Message) {
  try { channel?.postMessage(m); } catch { /* channel closed */ }
}

function broadcastChanged() {
  if (changedTimer) return;
  changedTimer = setTimeout(() => { changedTimer = null; broadcast({ type: "changed" }); }, 250);
}

/** The page's queue (created on first use). */
export function getStatusQueue(): StatusQueue {
  if (!queue) {
    const idb = IndexedDbQueueStorage.available();
    queue = new StatusQueue({
      storage: idb ? new IndexedDbQueueStorage() : new MemoryQueueStorage(),
      transport: createTransport(),
      persistent: idb,
      probeDuration: durationOf,
      notifyThrottleMs: 120,
      onPersist: broadcastChanged,
    });
  }
  return queue;
}

/**
 * Starts (or switches) the queue for this signed-in account. Idempotent; the queue keeps
 * running after the caller unmounts — only sign-out (`clearStatusQueue`) stops it.
 */
export function startStatusQueue(ownerId: string): void {
  if (typeof window === "undefined") return;
  if (ownerKey === ownerId) return;
  teardown?.();
  ownerKey = ownerId;
  const q = getStatusQueue();
  const cleanups: (() => void)[] = [];

  q.setOnline(navigator.onLine !== false);
  const online = () => q.setOnline(true);
  const offline = () => q.setOnline(false);
  const wake = () => { if (document.visibilityState === "visible") { q.setOnline(navigator.onLine !== false); q.kick(); } };
  window.addEventListener("online", online);
  window.addEventListener("offline", offline);
  document.addEventListener("visibilitychange", wake);
  window.addEventListener("pageshow", wake);
  cleanups.push(() => {
    window.removeEventListener("online", online);
    window.removeEventListener("offline", offline);
    document.removeEventListener("visibilitychange", wake);
    window.removeEventListener("pageshow", wake);
  });

  if (typeof BroadcastChannel !== "undefined") {
    channel = new BroadcastChannel(CHANNEL);
    channel.onmessage = (e: MessageEvent<Message>) => {
      const m = e.data;
      if (m?.type === "changed") void q.reload();
      else if (leader && m?.type === "retry") q.retry(m.id);
      else if (leader && m?.type === "cancel") void q.cancel(m.id);
    };
    cleanups.push(() => { channel?.close(); channel = null; });
  }

  const locks = (navigator as Navigator & { locks?: LockManager }).locks;
  if (locks?.request) {
    // Follower until this tab holds the lock; holding it until sign-out / tab close.
    void q.start(ownerId, false);
    const abort = new AbortController();
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    locks.request(`${CHANNEL}:${ownerId}`, { signal: abort.signal }, async () => {
      leader = true;
      await q.setActive(true);
      await held;
    }).catch(() => { /* aborted on sign-out / switch */ });
    cleanups.push(() => { abort.abort(); release(); leader = false; });
  } else {
    // No Web Locks (old browsers): this tab works the queue.
    leader = true;
    void q.start(ownerId, true);
    cleanups.push(() => { leader = false; });
  }

  teardown = () => {
    for (const c of cleanups.reverse()) c();
    q.stop();
    teardown = null;
    ownerKey = null;
  };
}

/** Queues a status (one or more ordered items) for background publishing. Resolves with the job id. */
export async function enqueueStatus(job: StatusJobInput): Promise<string> {
  const id = await getStatusQueue().enqueue(job);
  if (!leader) broadcast({ type: "changed" });
  return id;
}

export function retryStatus(id: string): void {
  if (leader || !channel) getStatusQueue().retry(id);
  else broadcast({ type: "retry", id });
}

export function cancelStatus(id: string): void {
  if (!leader && channel) broadcast({ type: "cancel", id });
  void getStatusQueue().cancel(id);
}

/** Lets an editor's deferred processing run (and re-run after a reload) in the background. */
export function registerStatusProcessor(name: string, fn: StatusProcessor): void {
  getStatusQueue().registerProcessor(name, fn);
}

/** Sign-out: stop, and erase every pending status and its media from this browser. */
export async function clearStatusQueue(): Promise<void> {
  teardown?.();
  if (queue) await queue.clearAll();
  else if (IndexedDbQueueStorage.available()) await new IndexedDbQueueStorage().clear().catch(() => {});
}
