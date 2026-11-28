/**
 * Unsent drafts stay with their chat (WhatsApp) — in THIS browser only, per account and chat
 * (`localStorage`). Nothing here is ever sent to a server, for any chat (private, end-to-end
 * encrypted chats included). The chat list subscribes so a row shows "Draft: …" as you type.
 * Signing out removes them (lib/messaging/signout.ts clears every `cfmeet:draft:` key).
 */

const PREFIX = "cfmeet:draft:";

export const draftStorageKey = (principalId: string, conversationId: string) => `${PREFIX}${principalId}:${conversationId}`;

let version = 0;
const listeners = new Set<() => void>();

function notify(): void {
  version++;
  for (const l of listeners) l();
}

/** The draft of one chat ("" without one, or without storage). */
export function readDraft(principalId: string | null | undefined, conversationId: string): string {
  if (!principalId || typeof window === "undefined") return "";
  try {
    return window.localStorage.getItem(draftStorageKey(principalId, conversationId)) ?? "";
  } catch {
    return "";
  }
}

/** Keeps (or, when empty, forgets) the draft of one chat; the chat list follows. */
export function writeDraft(principalId: string | null | undefined, conversationId: string, text: string): void {
  if (!principalId || typeof window === "undefined") return;
  const key = draftStorageKey(principalId, conversationId);
  try {
    const before = window.localStorage.getItem(key) ?? "";
    if (before === text) return;
    if (text) window.localStorage.setItem(key, text);
    else window.localStorage.removeItem(key);
  } catch {
    // Storage unavailable: drafts just aren't kept.
    return;
  }
  notify();
}

/** `useSyncExternalStore` source: bumps when any draft changes (this tab or another). */
function onStorage(e: StorageEvent): void {
  if (e.key === null || e.key.startsWith(PREFIX)) notify();
}

export function subscribeDrafts(listener: () => void): () => void {
  listeners.add(listener);
  if (listeners.size === 1 && typeof window !== "undefined") window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(listener);
    if (!listeners.size && typeof window !== "undefined") window.removeEventListener("storage", onStorage);
  };
}

export const draftsVersion = () => version;
