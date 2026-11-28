/**
 * Events (ADR 0071): owned by Messaging, reached through the Meeting Service's relay
 * (`/api/v2/messaging/relay/v1/messaging/events/...`) as the signed-in user's principal.
 * Every rule about who may see, answer or change an event is the server's; nothing here
 * filters events. The pure rules (bodies, queries, grids) are in `@/lib/events/core`.
 *
 * Cover images use the Messaging media store (purpose EVENT, images only, 8 MiB) exactly
 * like status media: the server answers part size and count, each part goes to protected
 * object storage through a signed single-object upload, then `complete`.
 */
import { messagingService, relay } from "./messaging.service";
import { MessagingApiError } from "@/lib/messaging/errors";
import type { MessagingMethod } from "@/lib/messaging/routes";
import {
  icsFilename,
  listQuery,
  type CalendarEvent,
  type EventCounts,
  type EventCreateRequest,
  type EventFilters,
  type EventPage,
  type EventRole,
  type EventUpdateRequest,
  type RsvpResponse,
} from "@/lib/events/core";

export type * from "@/lib/events/core";

const E = "v1/messaging/events";
const MEDIA = `${E}/media`;
const seg = (s: string) => encodeURIComponent(s);
/** Images only (ADR 0071 decision 10). */
export const COVER_MAX_BYTES = 8 * 1024 * 1024;

export interface EventAttendee {
  principalId: string;
  role: EventRole;
  response: RsvpResponse | null;
  respondedAt: string | null;
}
export interface EventAttendees { attendees: EventAttendee[]; counts: EventCounts | null }
export interface EventCalendarFile { filename: string; contentType: string; content: string }
export interface CoverMedia { mediaId: string; contentType: string; sizeBytes: number; partBytes: number; partCount: number; complete: boolean; storedParts?: number[] }

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Wire = any;

async function call<T>(method: MessagingMethod, path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
  return relay<T>(method, path, body, idempotencyKey);
}

/** The attendees answer, read tolerantly (`attendees` / `people` / `items`). */
function mapAttendees(w: Wire): EventAttendees {
  const list: Wire[] = w?.attendees ?? w?.people ?? w?.items ?? [];
  return {
    attendees: list.map((a) => ({ principalId: a.principalId, role: a.role ?? "INVITEE", response: a.response ?? null, respondedAt: a.respondedAt ?? null })),
    counts: w?.counts ?? null,
  };
}

// ---------------------------------------------------------------------------- cover upload

async function sha256Hex(data: ArrayBuffer): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", data);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function putPart(target: { url: string; headers: { name: string; value: string }[] }, body: ArrayBuffer, signal?: AbortSignal) {
  const headers = new Headers();
  for (const h of target.headers) if (h.name.toLowerCase() !== "content-length") headers.set(h.name, h.value);
  let r: Response;
  try {
    r = await fetch(target.url, { method: "PUT", headers, body, credentials: "omit", redirect: "error", signal });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new MessagingApiError(0, "UPLOAD_UNREACHABLE", null, "The upload didn't go through.");
  }
  if (!r.ok) throw new MessagingApiError(r.status, "UPLOAD_FAILED", null, "The upload didn't go through.");
}

/** Retries transient failures (network, 5xx, 429, an expired signed URL) with backoff. */
async function retrying<T>(signal: AbortSignal | undefined, attempt: () => Promise<T>): Promise<T> {
  for (let n = 0; ; n++) {
    if (signal?.aborted) throw Object.assign(new Error("Upload cancelled"), { name: "AbortError" });
    try {
      return await attempt();
    } catch (e) {
      const status = (e as { status?: number }).status ?? 0;
      const transient = status === 0 || status >= 500 || status === 429 || status === 403;
      if ((e as Error).name === "AbortError" || !transient || n >= 3) throw e;
      await new Promise((r) => setTimeout(r, Math.min(6000, 700 * 2 ** n)));
    }
  }
}

/** Upload a cover image in parts; `onProgress` gets 0-1. Cancel with `signal` (the server copy is removed). */
async function uploadCover(file: Blob, onProgress?: (p: number) => void, signal?: AbortSignal): Promise<CoverMedia> {
  if (!file.type.startsWith("image/")) throw new MessagingApiError(422, "UNSUPPORTED_MEDIA", null, "Choose a photo for the cover.");
  if (file.size > COVER_MAX_BYTES) throw new MessagingApiError(413, "PAYLOAD_TOO_LARGE", null, "The cover photo can be at most 8 MB.");
  const media = await call<CoverMedia>("POST", MEDIA, { kind: "IMAGE", contentType: file.type.split(";")[0], sizeBytes: file.size });
  if (media.complete) { onProgress?.(1); return media; }
  const onAbort = () => { void call("DELETE", `${MEDIA}/${seg(media.mediaId)}`).catch(() => {}); };
  signal?.addEventListener("abort", onAbort, { once: true });
  try {
    const stored = new Set(media.storedParts ?? []);
    let done = stored.size;
    for (let part = 0; part < media.partCount; part++) {
      if (stored.has(part)) continue;
      const bytes = await file.slice(part * media.partBytes, Math.min(file.size, (part + 1) * media.partBytes)).arrayBuffer();
      const sha256 = await sha256Hex(bytes);
      await retrying(signal, async () => {
        const grant = await call<{ stored: boolean; upload: { url: string; headers: { name: string; value: string }[] } | null }>(
          "POST", `${MEDIA}/${seg(media.mediaId)}/parts/${part}`, { sha256 });
        if (!grant.stored && grant.upload) await putPart(grant.upload, bytes, signal);
      });
      done += 1;
      onProgress?.(done / media.partCount);
    }
    return await retrying(signal, () => call<CoverMedia>("POST", `${MEDIA}/${seg(media.mediaId)}/complete`, {}));
  } finally {
    signal?.removeEventListener("abort", onAbort);
  }
}

const covers = new Map<string, Promise<string>>();

/** An object URL of a cover (downloaded once per page; the server authorizes every read). */
function coverUrl(mediaId: string): Promise<string> {
  let hit = covers.get(mediaId);
  if (!hit) {
    hit = (async () => {
      const grant = await call<{ contentType: string; parts: { part: number; url: string }[] }>("POST", `${MEDIA}/${seg(mediaId)}/download-grants`, {});
      const ordered = [...grant.parts].sort((a, b) => a.part - b.part);
      const parts = await Promise.all(ordered.map(async (p) => {
        const r = await fetch(p.url, { credentials: "omit", redirect: "error" });
        if (!r.ok) throw new MessagingApiError(r.status, "DOWNLOAD_FAILED", null);
        return r.arrayBuffer();
      }));
      return URL.createObjectURL(new Blob(parts, { type: grant.contentType || "image/jpeg" }));
    })();
    hit.catch(() => covers.delete(mediaId));
    covers.set(mediaId, hit);
  }
  return hit;
}

// ---------------------------------------------------------------------------- ICS

/** Save an `EventCalendarFile` (`content` is the iCalendar text) as a `.ics` download. */
export function saveCalendarFile(file: EventCalendarFile, fallback = "event") {
  const blob = new Blob([file.content], { type: file.contentType || "text/calendar" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = icsFilename(file.filename, fallback);
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

// ---------------------------------------------------------------------------- names

const names = new Map<string, string>();

/** Display names of principals (contacts, colleagues and, with `conversationIds`, chat members). */
async function resolveNames(principalIds: string[], conversationIds: string[] = []): Promise<Map<string, string>> {
  const wanted = [...new Set(principalIds.filter((p) => p && !names.has(p)))];
  for (let i = 0; i < wanted.length; i += 256) {
    try {
      const r = await messagingService.names(wanted.slice(i, i + 256), conversationIds);
      for (const p of r.principals) names.set(p.principalId, p.alias || p.displayName);
    } catch {
      // A lookup failure leaves people unnamed; it never fails the screen.
    }
  }
  return new Map(principalIds.filter((p) => names.has(p)).map((p) => [p, names.get(p)!]));
}

// ---------------------------------------------------------------------------- API

export const eventsService = {
  list: (filters: EventFilters) => call<EventPage>("GET", `${E}?${listQuery(filters)}`),
  /**
   * Create an event. Pass the same `idempotencyKey` on every retry of one user action
   * (24 h): the original event comes back instead of a duplicate.
   */
  create: (body: EventCreateRequest, idempotencyKey: string = crypto.randomUUID()) => call<CalendarEvent>("POST", E, body, idempotencyKey),
  get: (eventId: string) => call<CalendarEvent>("GET", `${E}/${seg(eventId)}`),
  update: (eventId: string, body: EventUpdateRequest) => call<CalendarEvent>("PATCH", `${E}/${seg(eventId)}`, body),
  remove: (eventId: string) => call<{ eventId: string; deleted?: boolean }>("DELETE", `${E}/${seg(eventId)}`),
  cancel: (eventId: string, reason?: string) => call<CalendarEvent>("POST", `${E}/${seg(eventId)}/cancel`, reason?.trim() ? { reason: reason.trim().slice(0, 500) } : {}),
  respond: (eventId: string, response: RsvpResponse) => call<CalendarEvent>("PUT", `${E}/${seg(eventId)}/rsvp`, { response }),
  attendees: async (eventId: string) => mapAttendees(await call<Wire>("GET", `${E}/${seg(eventId)}/attendees`)),
  invite: async (eventId: string, principalIds: string[]) => mapAttendees(await call<Wire>("POST", `${E}/${seg(eventId)}/invitations`, { principalIds })),
  removeInvitee: async (eventId: string, principalId: string) => mapAttendees(await call<Wire>("DELETE", `${E}/${seg(eventId)}/invitations/${seg(principalId)}`)),
  setReminders: async (eventId: string, minutesBefore: number[]) =>
    ((await call<{ minutesBefore?: number[] }>("PUT", `${E}/${seg(eventId)}/reminders`, { minutesBefore })).minutesBefore ?? minutesBefore),
  exportEvent: (eventId: string) => call<EventCalendarFile>("GET", `${E}/${seg(eventId)}/ics`),
  exportCalendar: () => call<EventCalendarFile>("GET", `${E}/calendar`),
  /** Mutual, unblocked contacts (the people a personal event may invite), named. */
  eligibleContacts: async (): Promise<{ principalId: string; name: string }[]> => {
    const r = await call<{ contacts: { principalId: string }[] }>("GET", "v1/messaging/status/eligible-contacts");
    const who = await resolveNames(r.contacts.map((c) => c.principalId));
    return r.contacts.map((c) => ({ principalId: c.principalId, name: who.get(c.principalId) ?? "Contact" }));
  },
  names: resolveNames,
  uploadCover,
  coverUrl,
  removeCover: async (mediaId: string) => { await call("DELETE", `${MEDIA}/${seg(mediaId)}`); },
  coverState: (mediaId: string) => call<CoverMedia>("GET", `${MEDIA}/${seg(mediaId)}`),
};

/** The problem with a Messaging events answer, in words. */
export function eventErrorMessage(error: unknown, fallback = "Something went wrong. Please try again."): string {
  if (error instanceof MessagingApiError) {
    if (error.code === "VERSION_CONFLICT" || (error.status === 409 && error.reason === "VERSION_CONFLICT")) return "Someone else changed this event while you were editing. Reload it to see their changes, then edit again.";
    if (error.reason === "EVENT_SCOPE_NOT_AVAILABLE") return "Organization events aren't available yet. Create a personal event, an event in a chat or a community event.";
    if (error.reason === "NOT_COMMUNITY_ADMIN") return "Only the community's admins can create its events.";
    if (error.status === 404) return "This event isn't available.";
    if (error.status === 409) return error.message && !/^[A-Z_0-9]+$/.test(error.message) ? error.message : "This event has been cancelled or has ended, so it can't be changed.";
    if (error.status === 503) return "Contacts are temporarily unavailable. Try again in a moment.";
    if (error.status === 429) return "You're doing that too often. Wait a moment and try again.";
    if (error.status === 401) return "Your session has expired. Sign in again to continue.";
    if (error.status === 0) return "You're offline or the connection dropped. Try again.";
    if (error.message && !/^[A-Z][A-Z0-9_]+$/.test(error.message)) return error.message;
    if (error.status === 403) return "You don't have permission to do that.";
    return fallback;
  }
  return error instanceof Error && error.message ? error.message : fallback;
}

export const isNotFound = (error: unknown) => error instanceof MessagingApiError && (error.status === 404 || error.code === "NOT_FOUND");
export const isVersionConflict = (error: unknown) => error instanceof MessagingApiError && (error.code === "VERSION_CONFLICT" || error.reason === "VERSION_CONFLICT");
