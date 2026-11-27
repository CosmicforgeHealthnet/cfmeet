/**
 * Queue persistence: IndexedDB in the browser (job records + media Blobs, so pending statuses
 * survive reloads and restarts), memory as the fallback (private modes without IndexedDB) and
 * in tests.
 *
 * Pure (type-only imports): node tests import this file directly.
 */
import type { QueueStorage, StoredJob } from "./types";

const DB_NAME = "cfmeet-status-queue";
const DB_VERSION = 1;
const JOBS = "jobs";
const BLOBS = "blobs";

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction aborted"));
  });
}

export class IndexedDbQueueStorage implements QueueStorage {
  private db: Promise<IDBDatabase> | null = null;

  private readonly name: string;

  constructor(name = DB_NAME) {
    this.name = name;
  }

  static available(): boolean {
    return typeof indexedDB !== "undefined";
  }

  private open(): Promise<IDBDatabase> {
    if (!this.db) {
      this.db = new Promise<IDBDatabase>((resolve, reject) => {
        const req = indexedDB.open(this.name, DB_VERSION);
        req.onupgradeneeded = () => {
          const db = req.result;
          if (!db.objectStoreNames.contains(JOBS)) db.createObjectStore(JOBS, { keyPath: "id" });
          if (!db.objectStoreNames.contains(BLOBS)) db.createObjectStore(BLOBS);
        };
        req.onsuccess = () => {
          const db = req.result;
          // Another tab upgrading: let it, and reopen next time.
          db.onversionchange = () => { db.close(); this.db = null; };
          resolve(db);
        };
        req.onerror = () => reject(req.error);
        req.onblocked = () => reject(new Error("IndexedDB blocked"));
      });
      this.db.catch(() => { this.db = null; });
    }
    return this.db;
  }

  private async tx(stores: string[], mode: IDBTransactionMode): Promise<IDBTransaction> {
    return (await this.open()).transaction(stores, mode);
  }

  async loadJobs(): Promise<StoredJob[]> {
    const tx = await this.tx([JOBS], "readonly");
    return request(tx.objectStore(JOBS).getAll() as IDBRequest<StoredJob[]>);
  }

  async putJob(job: StoredJob): Promise<void> {
    const tx = await this.tx([JOBS], "readwrite");
    tx.objectStore(JOBS).put(job);
    await done(tx);
  }

  async deleteJob(id: string): Promise<void> {
    const tx = await this.tx([JOBS], "readwrite");
    tx.objectStore(JOBS).delete(id);
    await done(tx);
  }

  async putBlob(key: string, blob: Blob): Promise<void> {
    const tx = await this.tx([BLOBS], "readwrite");
    tx.objectStore(BLOBS).put(blob, key);
    await done(tx);
  }

  async getBlob(key: string): Promise<Blob | null> {
    const tx = await this.tx([BLOBS], "readonly");
    const b = await request(tx.objectStore(BLOBS).get(key) as IDBRequest<Blob | undefined>);
    return b ?? null;
  }

  async deleteBlob(key: string): Promise<void> {
    const tx = await this.tx([BLOBS], "readwrite");
    tx.objectStore(BLOBS).delete(key);
    await done(tx);
  }

  async deleteBlobs(jobId: string): Promise<void> {
    const tx = await this.tx([BLOBS], "readwrite");
    tx.objectStore(BLOBS).delete(IDBKeyRange.bound(`${jobId}/`, `${jobId}/￿`));
    await done(tx);
  }

  async clear(): Promise<void> {
    const tx = await this.tx([JOBS, BLOBS], "readwrite");
    tx.objectStore(JOBS).clear();
    tx.objectStore(BLOBS).clear();
    await done(tx);
  }
}

/** In-memory storage: the fallback without IndexedDB, and a "device" that survives a simulated reload in tests. */
export class MemoryQueueStorage implements QueueStorage {
  readonly jobs = new Map<string, StoredJob>();
  readonly blobs = new Map<string, Blob>();

  async loadJobs(): Promise<StoredJob[]> { return [...this.jobs.values()].map((j) => JSON.parse(JSON.stringify(j)) as StoredJob); }
  async putJob(job: StoredJob): Promise<void> { this.jobs.set(job.id, JSON.parse(JSON.stringify(job)) as StoredJob); }
  async deleteJob(id: string): Promise<void> { this.jobs.delete(id); }
  async putBlob(key: string, blob: Blob): Promise<void> { this.blobs.set(key, blob); }
  async getBlob(key: string): Promise<Blob | null> { return this.blobs.get(key) ?? null; }
  async deleteBlob(key: string): Promise<void> { this.blobs.delete(key); }
  async deleteBlobs(jobId: string): Promise<void> { for (const k of [...this.blobs.keys()]) if (k.startsWith(`${jobId}/`)) this.blobs.delete(k); }
  async clear(): Promise<void> { this.jobs.clear(); this.blobs.clear(); }
}
