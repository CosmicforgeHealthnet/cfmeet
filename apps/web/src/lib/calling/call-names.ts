/**
 * Names for calls this browser placed to a contact. Calling and the names lookup don't know
 * personal contacts, so the caller remembers who it dialled (kept for this browser session).
 */
const KEY = "cfmeet.call-names";

function read(): Record<string, string> {
  try { return JSON.parse(sessionStorage.getItem(KEY) ?? "{}") as Record<string, string>; } catch { return {}; }
}

export function rememberCallName(callId: string, name: string | null | undefined) {
  if (!callId || !name) return;
  try { sessionStorage.setItem(KEY, JSON.stringify({ ...read(), [callId]: name })); } catch { /* storage unavailable */ }
}

export function rememberedCallName(callId: string | null | undefined): string | null {
  return callId ? read()[callId] ?? null : null;
}
