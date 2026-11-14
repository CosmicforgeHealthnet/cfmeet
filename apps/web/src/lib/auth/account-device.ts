/**
 * Browser binding of the account device (see account-device-core.ts): IndexedDB storage of
 * the non-extractable key and the Account authority's device endpoints.
 */
import { createAccountDevice, type StoredAccountDevice } from "./account-device-core";

const DB = "cfmeet-account-device";
const STORE = "device";
const CURRENT = "current";
const accountKey = (account: string) => `account:${account}`;

function db(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(DB, 1);
    open.onupgradeneeded = () => open.result.createObjectStore(STORE);
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
}

async function get<T>(key: string): Promise<T | null> {
  if (typeof indexedDB === "undefined") return null;
  const d = await db();
  return new Promise((resolve, reject) => {
    const q = d.transaction(STORE, "readonly").objectStore(STORE).get(key);
    q.onsuccess = () => resolve((q.result as T | undefined) ?? null);
    q.onerror = () => reject(q.error);
  });
}

async function put(key: string, value: unknown): Promise<void> {
  const d = await db();
  await new Promise<void>((resolve, reject) => {
    const tx = d.transaction(STORE, "readwrite");
    tx.objectStore(STORE).put(value, key);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// Lazy: the v2 client depends on the session module, which depends on this module.
const v2 = async () => (await import("@/services/v2/api")).default;

const browser = () => typeof window !== "undefined" && typeof crypto !== "undefined" && !!crypto.subtle;

export const accountDevice = createAccountDevice({
  subtle: browser() ? crypto.subtle : ({} as SubtleCrypto),
  randomBytes: (n) => crypto.getRandomValues(new Uint8Array(n)),
  load: (account) => (browser() ? get<StoredAccountDevice>(accountKey(account)).catch(() => null) : Promise.resolve(null)),
  save: (account, device) => put(accountKey(account), device),
  loadCurrent: () => (browser() ? get<string>(CURRENT).catch(() => null) : Promise.resolve(null)),
  saveCurrent: (account) => put(CURRENT, account),
  api: {
    declare: async (body) => (await (await v2()).post("/me/devices/current", body)).data,
    createLink: async (body) => (await (await v2()).post("/me/device-links", body)).data,
    getLink: async (id) => (await (await v2()).get(`/me/device-links/${id}`)).data,
    completeLink: async (id, body) => (await (await v2()).post(`/me/device-links/${id}/complete`, body)).data,
    pendingLinks: async () => (await (await v2()).get("/me/device-links")).data,
    approveLink: async (id, body) => (await (await v2()).post(`/me/device-links/${id}/approve`, body)).data,
    rejectLink: async (id) => (await (await v2()).post(`/me/device-links/${id}/reject`, {})).data,
  },
});
