// Recent photos and videos for the status creation screen. Browsers never expose the phone's
// gallery to a web page, so this uses the best each one offers:
// * a file input with `multiple` (on phones it opens the system photo picker, newest first);
// * where the File System Access API exists (desktop Chromium), a folder the person chooses
//   ("Pictures", "Camera"…): its handle is remembered in IndexedDB, so the grid shows that
//   folder's newest photos and videos again next time (after the browser's permission check);
// * elsewhere a folder input (`webkitdirectory`).
// Thumbnails are made small (≤ 320 px) in the browser, a few at a time.

/** Media file extensions recognised when the file has no MIME type (folder listings). */
const IMAGE_EXT = /\.(jpe?g|png|webp|gif|heic|heif|avif|bmp)$/i;
const VIDEO_EXT = /\.(mp4|m4v|mov|webm|mkv|3gp)$/i;

export type RecentKind = "IMAGE" | "VIDEO";

/** IMAGE / VIDEO for a photo or video file (by type, else by name), or null. */
export function recentKindOf(file: { name: string; type: string }): RecentKind | null {
  if (file.type.startsWith("image/")) return "IMAGE";
  if (file.type.startsWith("video/")) return "VIDEO";
  if (!file.type || file.type === "application/octet-stream") {
    if (IMAGE_EXT.test(file.name)) return "IMAGE";
    if (VIDEO_EXT.test(file.name)) return "VIDEO";
  }
  return null;
}

/** Newest first, de-duplicated by name + size + date (the same file picked twice), at most `max`. */
export function newestMedia<T extends { name: string; size: number; lastModified: number; type: string }>(files: readonly T[], max = 60): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const f of [...files].sort((a, b) => b.lastModified - a.lastModified)) {
    if (!recentKindOf(f)) continue;
    const key = `${f.name}|${f.size}|${f.lastModified}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(f);
    if (out.length >= max) break;
  }
  return out;
}

// ---------------------------------------------------------------- File System Access

type DirHandle = FileSystemDirectoryHandle & {
  values(): AsyncIterable<FileSystemHandle>;
  queryPermission?(o: { mode: "read" }): Promise<PermissionState>;
  requestPermission?(o: { mode: "read" }): Promise<PermissionState>;
};
type PickerWindow = Window & { showDirectoryPicker?: (o?: { id?: string; mode?: "read"; startIn?: string }) => Promise<DirHandle> };

/** The browser can open a folder and remember it (File System Access). */
export function canRememberFolder(): boolean {
  return typeof window !== "undefined" && typeof (window as PickerWindow).showDirectoryPicker === "function";
}

/** The browser can pick a folder through a file input. */
export function canPickFolderInput(): boolean {
  if (typeof document === "undefined") return false;
  return "webkitdirectory" in document.createElement("input");
}

const DB = "cfmeet-status-recent";
const STORE = "folders";
const KEY = "status-folder";

function idb<T>(mode: IDBTransactionMode, run: (s: IDBObjectStore) => IDBRequest<T> | void): Promise<T | undefined> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") { reject(new Error("No IndexedDB")); return; }
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => { req.result.createObjectStore(STORE); };
    req.onerror = () => reject(req.error);
    req.onsuccess = () => {
      const db = req.result;
      const t = db.transaction(STORE, mode);
      const r = run(t.objectStore(STORE));
      t.oncomplete = () => { db.close(); resolve(r ? r.result : undefined); };
      t.onerror = () => { db.close(); reject(t.error); };
    };
  });
}

/** The remembered folder, and whether the browser still lets the page read it without asking. */
export async function rememberedFolder(): Promise<{ handle: DirHandle; name: string; granted: boolean } | null> {
  if (!canRememberFolder()) return null;
  try {
    const handle = (await idb<DirHandle>("readonly", (s) => s.get(KEY) as IDBRequest<DirHandle>)) ?? null;
    if (!handle) return null;
    const state = handle.queryPermission ? await handle.queryPermission({ mode: "read" }) : "granted";
    return { handle, name: handle.name, granted: state === "granted" };
  } catch {
    return null;
  }
}

/** Asks for a folder (user gesture), remembers it, and returns its newest media. */
export async function chooseFolder(): Promise<{ name: string; files: File[] } | null> {
  const w = window as PickerWindow;
  if (!w.showDirectoryPicker) return null;
  let handle: DirHandle;
  try {
    handle = await w.showDirectoryPicker({ id: "cfmeet-status", mode: "read", startIn: "pictures" });
  } catch {
    return null; // cancelled
  }
  await idb("readwrite", (s) => { s.put(handle, KEY); }).catch(() => {});
  return { name: handle.name, files: await listFolder(handle) };
}

/** Re-grants read access to the remembered folder (user gesture) and lists it. */
export async function reopenFolder(handle: DirHandle): Promise<File[] | null> {
  try {
    const state = handle.requestPermission ? await handle.requestPermission({ mode: "read" }) : "granted";
    return state === "granted" ? await listFolder(handle) : null;
  } catch {
    return null;
  }
}

export async function forgetFolder(): Promise<void> {
  await idb("readwrite", (s) => { s.delete(KEY); }).catch(() => {});
}

/** Photos and videos in a folder and its direct subfolders, newest first (bounded scan). */
export async function listFolder(handle: DirHandle, max = 60): Promise<File[]> {
  const files: File[] = [];
  let scanned = 0;
  const walk = async (dir: DirHandle, depth: number) => {
    for await (const entry of dir.values()) {
      if (++scanned > 3000) return;
      if (entry.kind === "file") {
        if (!IMAGE_EXT.test(entry.name) && !VIDEO_EXT.test(entry.name)) continue;
        try { files.push(await (entry as FileSystemFileHandle).getFile()); } catch { /* unreadable */ }
      } else if (depth < 1 && !entry.name.startsWith(".")) {
        await walk(entry as DirHandle, depth + 1);
      }
    }
  };
  await walk(handle, 0);
  return newestMedia(files, max);
}

// ---------------------------------------------------------------- thumbnails

export type Thumb = { url: string; durationMs: number | null };

const THUMB_PX = 320;
const cache = new WeakMap<Blob, Promise<Thumb>>();
let running = 0;
const waiting: (() => void)[] = [];
async function slot<T>(fn: () => Promise<T>): Promise<T> {
  if (running >= 3) await new Promise<void>((r) => waiting.push(r));
  running++;
  try { return await fn(); } finally { running--; waiting.shift()?.(); }
}

async function canvasBlob(source: CanvasImageSource, w: number, h: number): Promise<Blob> {
  const k = Math.min(1, THUMB_PX / Math.max(w, h));
  const cw = Math.max(1, Math.round(w * k));
  const ch = Math.max(1, Math.round(h * k));
  const c = document.createElement("canvas");
  c.width = cw;
  c.height = ch;
  c.getContext("2d")?.drawImage(source, 0, 0, cw, ch);
  return new Promise((resolve, reject) => c.toBlob((b) => (b ? resolve(b) : reject(new Error("thumbnail"))), "image/jpeg", 0.72));
}

function videoThumb(file: Blob): Promise<Thumb> {
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    const url = URL.createObjectURL(file);
    let done = false;
    const finish = (r: Thumb | null, e?: unknown) => {
      if (done) return;
      done = true;
      v.removeAttribute("src");
      v.load();
      URL.revokeObjectURL(url);
      if (r) resolve(r); else reject(e ?? new Error("video thumbnail"));
    };
    v.muted = true;
    v.playsInline = true;
    v.preload = "metadata";
    v.onloadedmetadata = () => { v.currentTime = Math.min(0.5, (v.duration || 1) / 3); };
    v.onseeked = () => {
      const durationMs = Number.isFinite(v.duration) ? Math.round(v.duration * 1000) : null;
      canvasBlob(v, v.videoWidth || 16, v.videoHeight || 9).then((b) => finish({ url: URL.createObjectURL(b), durationMs }), () => finish({ url: "", durationMs }));
    };
    v.onerror = () => finish(null, new Error("video thumbnail"));
    setTimeout(() => finish(null, new Error("timeout")), 15_000);
    v.src = url;
  });
}

async function imageThumb(file: Blob): Promise<Thumb> {
  const bmp = await createImageBitmap(file, { imageOrientation: "from-image" });
  try {
    return { url: URL.createObjectURL(await canvasBlob(bmp, bmp.width, bmp.height)), durationMs: null };
  } finally {
    bmp.close();
  }
}

/** A small JPEG thumbnail (object URL, kept for the page's life) and a video's duration. */
export function thumbnailOf(file: File): Promise<Thumb> {
  let hit = cache.get(file);
  if (!hit) {
    hit = slot(() => (recentKindOf(file) === "VIDEO" ? videoThumb(file) : imageThumb(file)));
    cache.set(file, hit);
  }
  return hit;
}
