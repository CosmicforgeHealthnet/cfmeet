// "My music": the audio files this person recently used for statuses, kept on THIS device only
// (IndexedDB) so they can be picked again without searching their files. Nothing is uploaded
// until a status uses a clip of it, and no music service is involved: CF Meet offers only the
// user's own audio. Pure pruning rules are exported for tests; IndexedDB is touched only
// inside the async functions.

export type LibraryTrack = {
  id: string;
  title: string;
  /** The original file. */
  blob: Blob;
  type: string;
  sizeBytes: number;
  durationMs: number;
  addedAt: number;
  lastUsedAt: number;
};

export type TrackMeta = Omit<LibraryTrack, "blob">;

/** Tracks kept at most, and total bytes kept at most (the oldest-used go first). */
export const LIBRARY_MAX_TRACKS = 15;
export const LIBRARY_MAX_BYTES = 200 * 1024 * 1024;

/** Ids to drop so the library fits the limits, least recently used first. */
export function tracksToPrune(tracks: readonly Pick<TrackMeta, "id" | "sizeBytes" | "lastUsedAt">[], maxTracks = LIBRARY_MAX_TRACKS, maxBytes = LIBRARY_MAX_BYTES): string[] {
  const byRecent = [...tracks].sort((a, b) => b.lastUsedAt - a.lastUsedAt);
  const drop: string[] = [];
  let bytes = 0;
  byRecent.forEach((t, i) => {
    bytes += t.sizeBytes;
    if (i >= maxTracks || (i > 0 && bytes > maxBytes)) { drop.push(t.id); bytes -= t.sizeBytes; }
  });
  return drop;
}

/** Tracks whose title contains every word of `query` (case and accents ignored), most recent first. */
export function searchTracks<T extends Pick<TrackMeta, "title" | "lastUsedAt">>(tracks: readonly T[], query: string): T[] {
  const fold = (s: string) => s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();
  const words = fold(query).split(/\s+/).filter(Boolean);
  return tracks.filter((t) => words.every((w) => fold(t.title).includes(w))).sort((a, b) => b.lastUsedAt - a.lastUsedAt);
}

/** The same file picked again (same name, size and type) is one library entry. */
export function trackKey(title: string, sizeBytes: number, type: string): string {
  return `${title}|${sizeBytes}|${type}`;
}

const DB = "cfmeet-status-music";
const STORE = "tracks";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") { reject(new Error("No IndexedDB")); return; }
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => { req.result.createObjectStore(STORE, { keyPath: "id" }); };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function tx<T>(mode: IDBTransactionMode, run: (s: IDBObjectStore) => IDBRequest<T> | void): Promise<T | undefined> {
  return open().then((db) => new Promise<T | undefined>((resolve, reject) => {
    const t = db.transaction(STORE, mode);
    const req = run(t.objectStore(STORE));
    t.oncomplete = () => { db.close(); resolve(req ? req.result : undefined); };
    t.onerror = () => { db.close(); reject(t.error); };
    t.onabort = () => { db.close(); reject(t.error); };
  }));
}

/** Every track, most recently used first (empty when IndexedDB is unavailable). */
export async function listTracks(): Promise<LibraryTrack[]> {
  try {
    const all = (await tx<LibraryTrack[]>("readonly", (s) => s.getAll() as IDBRequest<LibraryTrack[]>)) ?? [];
    return all.sort((a, b) => b.lastUsedAt - a.lastUsedAt);
  } catch {
    return [];
  }
}

/** Adds (or refreshes) a track and prunes the library. Best effort: returns the entry even when it can't be stored. */
export async function saveTrack(file: Blob, title: string, durationMs: number): Promise<LibraryTrack> {
  const now = Date.now();
  const id = trackKey(title, file.size, file.type);
  const track: LibraryTrack = { id, title, blob: file, type: file.type, sizeBytes: file.size, durationMs: Math.round(durationMs), addedAt: now, lastUsedAt: now };
  try {
    await tx("readwrite", (s) => { s.put(track); });
    const all = await listTracks();
    const drop = tracksToPrune(all);
    if (drop.length) await tx("readwrite", (s) => { for (const d of drop) s.delete(d); });
  } catch {
    /* private mode / quota: the track still works for this status */
  }
  return track;
}

/** Marks a track as just used. */
export async function touchTrack(id: string): Promise<void> {
  try {
    const all = await listTracks();
    const t = all.find((x) => x.id === id);
    if (t) await tx("readwrite", (s) => { s.put({ ...t, lastUsedAt: Date.now() }); });
  } catch { /* best effort */ }
}

export async function deleteTrack(id: string): Promise<void> {
  try { await tx("readwrite", (s) => { s.delete(id); }); } catch { /* best effort */ }
}
