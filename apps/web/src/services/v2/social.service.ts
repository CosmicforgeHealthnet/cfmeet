/**
 * Updates (WhatsApp-style): Status (24 hours, server-enforced audience) and Channels.
 *
 * Owned by the Messaging service (ADR 0069) and reached through the Meeting Service's
 * Messaging relay (`/api/v2/messaging/relay/v1/messaging/status/...` and `.../channels/...`),
 * acting as the signed-in user's own principal. Every audience rule is enforced by the
 * server; nothing here filters who may see what.
 *
 * Media goes straight to protected object storage in parts (signed single-object uploads
 * the server mints per part): resumable (`uploadState` lists the parts the server holds),
 * idempotent per part, a few parts in parallel with retries, cancellable (`cancelUpload`
 * or an AbortSignal). Reads use short-lived signed part URLs the server mints only for
 * authorized readers.
 */
import { messagingService, relay } from "./messaging.service";
import { MessagingApiError } from "@/lib/messaging/errors";

export type MediaKind = "IMAGE" | "VIDEO" | "AUDIO";
export type PostKind = "TEXT" | MediaKind;
export type MediaPurpose = "STATUS" | "CHANNEL";
export type AudienceMode = "CONTACTS" | "CONTACTS_EXCEPT" | "ONLY" | "CLOSE_FRIENDS" | "CUSTOM";

export interface MediaView {
  mediaId: string;
  kind: MediaKind;
  contentType: string;
  sizeBytes: number;
  /** Every part but the last has exactly this many bytes. */
  chunkBytes: number;
  chunkCount: number;
  durationMs: number | null;
  complete: boolean;
  /** Which media API serves it (status or channel). */
  purpose: MediaPurpose;
  /** Owner only, while uploading: the parts the server already holds. */
  storedParts?: number[];
}

export interface PersonView {
  /** The person's Messaging principal (statuses and channels know people by principal). */
  userId: string;
  name: string;
  avatarUrl: string | null;
  principalId: string | null;
}

export interface Gradient { from: string; to: string; angle: number }

export interface StatusAudienceInfo { mode: AudienceMode; label: string | null; listId: string | null; includedCount: number; excludedCount: number }

export interface StatusViewer {
  person: PersonView;
  viewedAt: string;
  liked: boolean;
  reaction?: string | null;
}

export interface StatusPost {
  postId: string;
  statusId: string;
  authorPrincipalId: string;
  kind: PostKind;
  text: string | null;
  background: string | null;
  font: number;
  media: MediaView | null;
  startMs: number | null;
  endMs: number | null;
  caption: string | null;
  createdAt: string;
  expiresAt: string;
  viewed: boolean;
  liked: boolean;
  viewCount: number | null;
  textColor?: string | null;
  textAlign?: "left" | "center" | "right" | null;
  textSize?: number | null;
  backgroundGradient?: Gradient | null;
  music?: { mediaId: string; media: MediaView | null; startMs: number | null; endMs: number | null; title: string | null } | null;
  musicTitle?: string | null;
  groupId?: string | null;
  sequence?: number | null;
  remainingSeconds?: number;
  reaction?: string | null;
  mentionedMe?: boolean;
  closeFriends?: boolean;
  allowSharing?: boolean;
  origin?: { statusId: string; authorPrincipalId: string } | null;
  /** The author's own statuses only. */
  audience?: StatusAudienceInfo;
  mentions?: string[];
  shareCount?: number;
  uniqueViewerCount?: number | null;
  viewers?: StatusViewer[];
}

export interface StatusAuthor {
  person: PersonView;
  posts: StatusPost[];
  allViewed: boolean;
  lastPostedAt: string;
  closeFriends?: boolean;
}

export interface StatusFeed {
  mine: StatusPost[];
  contacts: StatusAuthor[];
  /** Authors the caller muted (not shown in the main row). */
  muted: StatusAuthor[];
}

export interface ChannelPost {
  postId: string;
  channelId: string;
  kind: PostKind;
  text: string | null;
  media: MediaView | null;
  createdAt: string;
  reactions: { emoji: string; count: number }[];
  myReaction: string | null;
}

export interface Channel {
  channelId: string;
  name: string;
  description: string | null;
  owner: PersonView;
  followerCount: number;
  following: boolean;
  isOwner: boolean;
  unreadCount: number;
  lastPost: ChannelPost | null;
  createdAt: string;
}

export interface AudienceChoice { mode: AudienceMode; principalIds?: string[]; listId?: string }

/** Richer optional fields of a new status (validated by the server). */
export interface StatusExtras {
  caption?: string;
  textColor?: string;
  textAlign?: "left" | "center" | "right";
  textSize?: number;
  background?: string;
  backgroundGradient?: Gradient;
  font?: number;
  musicMediaId?: string;
  musicStartMs?: number;
  musicEndMs?: number;
  musicTitle?: string;
  groupId?: string;
  sequence?: number;
  mentions?: string[];
  audience?: AudienceChoice;
  allowSharing?: boolean;
}

export type CreateStatus =
  | ({ kind: "TEXT"; text: string } & StatusExtras)
  | ({ kind: "IMAGE" | "AUDIO"; mediaId: string } & StatusExtras)
  | ({ kind: "VIDEO"; mediaId: string; startMs?: number; endMs?: number } & StatusExtras);

export interface AudiencePreference { mode: AudienceMode; principalIds: string[]; listId: string | null; allowSharing: boolean }
export interface AudienceList { listId: string; kind: "CLOSE_FRIENDS" | "CUSTOM"; name: string; memberPrincipalIds: string[]; memberCount: number; updatedAt: string }
export interface StatusRing { principalId: string; hasActiveStatus: boolean; count: number; unseenCount: number; closeFriends: boolean }

export const SEGMENT_MS = 60_000;
export const LIMITS: Record<MediaKind, number> = { IMAGE: 8 * 1024 * 1024, AUDIO: 8 * 1024 * 1024, VIDEO: 64 * 1024 * 1024 };
/** Parts uploaded at once. */
export const UPLOAD_CONCURRENCY = 3;

type Method = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
// Wire shapes are Messaging's (cfmeet.messaging.http-api); mapped below to the screens' types.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Wire = any;

const S = "v1/messaging/status";
const CH = "v1/messaging/channels";
const id = (s: string) => encodeURIComponent(s);
const uuid = () => crypto.randomUUID();
const mediaBase = (purpose: MediaPurpose) => (purpose === "STATUS" ? `${S}/media` : `${CH}/media`);

/** Errors keep the `response.data.error` shape the Updates screens read. */
async function call<T>(method: Method, path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
  try {
    return await relay<T>(method, path, body, idempotencyKey);
  } catch (e) {
    if (e instanceof MessagingApiError) {
      throw Object.assign(e, { response: { status: e.status, data: { error: { code: e.code, reason: e.reason, message: e.message !== e.code ? e.message : undefined } } } });
    }
    throw e;
  }
}

// ------------------------------------------------------------------------------ people

const names = new Map<string, string>();

/** Display names come from the existing profile / contact names API (display-only). */
async function people(principals: string[]): Promise<Map<string, PersonView>> {
  const wanted = [...new Set(principals.filter((p) => p && !names.has(p)))];
  for (let i = 0; i < wanted.length; i += 256) {
    try {
      const r = await messagingService.names(wanted.slice(i, i + 256));
      for (const p of r.principals) names.set(p.principalId, p.alias || p.displayName);
    } catch {
      // A lookup failure leaves people unnamed; it never fails the screen.
    }
  }
  return new Map(principals.map((p) => [p, person(p)]));
}

function person(p: string, fallback = "Contact"): PersonView {
  return { userId: p, principalId: p, name: names.get(p) ?? fallback, avatarUrl: null };
}

// ------------------------------------------------------------------------------ mapping

function mapMedia(m: Wire, purpose: MediaPurpose): MediaView | null {
  if (!m) return null;
  return {
    mediaId: m.mediaId, kind: m.kind, contentType: m.contentType, sizeBytes: m.sizeBytes,
    chunkBytes: m.partBytes, chunkCount: m.partCount, durationMs: m.durationMs ?? null,
    complete: !!m.complete, purpose, storedParts: m.storedParts,
  };
}

function mapViewer(v: Wire): StatusViewer {
  return { person: person(v.principalId), viewedAt: v.viewedAt, liked: !!v.liked, reaction: v.reaction ?? null };
}

function mapStatus(s: Wire): StatusPost {
  return {
    postId: s.statusId, statusId: s.statusId, authorPrincipalId: s.authorPrincipalId, kind: s.kind,
    text: s.text ?? null, background: s.background ?? s.backgroundGradient?.from ?? null, font: s.font ?? 0,
    media: mapMedia(s.media, "STATUS"), startMs: s.startMs ?? null, endMs: s.endMs ?? null, caption: s.caption ?? null,
    createdAt: s.createdAt, expiresAt: s.expiresAt, viewed: !!s.viewed, liked: !!s.liked,
    viewCount: s.viewCount ?? null, textColor: s.textColor ?? null, textAlign: s.textAlign ?? null, textSize: s.textSize ?? null,
    backgroundGradient: s.backgroundGradient ?? null,
    music: s.music ? { mediaId: s.music.mediaId, media: mapMedia(s.music.media, "STATUS"), startMs: s.music.startMs ?? null, endMs: s.music.endMs ?? null, title: s.music.title ?? null } : null,
    musicTitle: s.musicTitle ?? null, groupId: s.groupId ?? null, sequence: s.sequence ?? null, remainingSeconds: s.remainingSeconds,
    reaction: s.reaction ?? null, mentionedMe: !!s.mentionedMe, closeFriends: !!s.closeFriends, allowSharing: s.allowSharing !== false,
    origin: s.origin ?? null, audience: s.audience, mentions: s.mentions, shareCount: s.shareCount,
    uniqueViewerCount: s.uniqueViewerCount, viewers: Array.isArray(s.viewers) ? s.viewers.map(mapViewer) : undefined,
  };
}

function mapPost(p: Wire): ChannelPost {
  return { postId: p.postId, channelId: p.channelId, kind: p.kind, text: p.text ?? null, media: mapMedia(p.media, "CHANNEL"), createdAt: p.createdAt, reactions: p.reactions ?? [], myReaction: p.myReaction ?? null };
}

async function mapChannels(list: Wire[]): Promise<Channel[]> {
  await people(list.map((c) => c.ownerPrincipalId));
  return list.map((c) => ({
    channelId: c.channelId, name: c.name, description: c.description ?? null, owner: person(c.ownerPrincipalId, "Channel owner"),
    followerCount: c.followerCount, following: !!c.following, isOwner: !!c.isOwner, unreadCount: c.unreadCount ?? 0,
    lastPost: c.lastPost ? mapPost(c.lastPost) : null, createdAt: c.createdAt,
  }));
}

async function mapAuthors(list: Wire[]): Promise<StatusAuthor[]> {
  await people(list.map((a) => a.authorPrincipalId));
  return list.map((a) => ({
    person: person(a.authorPrincipalId), posts: (a.posts as Wire[]).map(mapStatus),
    allViewed: !!a.allViewed, lastPostedAt: a.lastPostedAt, closeFriends: !!a.closeFriends,
  }));
}

// ------------------------------------------------------------------------------ uploads

export interface UploadOptions {
  /** Resume this earlier upload (only the parts the server lacks are sent). */
  mediaId?: string;
  /** Cancels the upload (the server copy is removed too). */
  signal?: AbortSignal;
  concurrency?: number;
  /** Called once the server created the upload (persist `mediaId` to resume later). */
  onStarted?: (media: MediaView) => void;
}

async function sha256Hex(data: ArrayBuffer): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", data);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function throwIfAborted(signal?: AbortSignal) {
  if (signal?.aborted) throw Object.assign(new Error("Upload cancelled"), { name: "AbortError" });
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
    throwIfAborted(signal);
    try {
      return await attempt();
    } catch (e) {
      const status = (e as { status?: number }).status ?? 0;
      const transient = status === 0 || status >= 500 || status === 429 || status === 403;
      if ((e as Error).name === "AbortError" || !transient || n >= 4) throw e;
      await new Promise((r) => setTimeout(r, Math.min(8000, 700 * 2 ** n)));
    }
  }
}

/** The server's view of an upload: `storedParts` says which parts it already holds. */
async function uploadState(mediaId: string, purpose: MediaPurpose = "STATUS"): Promise<MediaView> {
  return mapMedia(await call<Wire>("GET", `${mediaBase(purpose)}/${id(mediaId)}`), purpose)!;
}

/** Cancel an upload (or remove unused media of the caller) with its stored bytes. */
async function cancelUpload(mediaId: string, purpose: MediaPurpose = "STATUS"): Promise<void> {
  await call("DELETE", `${mediaBase(purpose)}/${id(mediaId)}`);
}

/** Start an upload (the server answers the part size and count). */
async function startUpload(purpose: MediaPurpose, kind: MediaKind, contentType: string, sizeBytes: number, durationMs: number | null): Promise<MediaView> {
  return mapMedia(await call<Wire>("POST", mediaBase(purpose), { kind, contentType, sizeBytes, durationMs: durationMs === null ? undefined : Math.round(durationMs) }), purpose)!;
}

/**
 * Send one part (idempotent: the same bytes again are answered "already stored"; different
 * bytes for a stored part are a 409). Parts are independent, so several may run at once.
 */
async function uploadPart(mediaId: string, index: number, body: Blob, purpose: MediaPurpose = "STATUS", signal?: AbortSignal): Promise<void> {
  const bytes = await body.arrayBuffer();
  const sha256 = await sha256Hex(bytes);
  throwIfAborted(signal);
  const grant = await call<{ stored: boolean; upload: { url: string; headers: { name: string; value: string }[] } | null }>(
    "POST", `${mediaBase(purpose)}/${id(mediaId)}/parts/${index}`, { sha256 });
  if (!grant.stored && grant.upload) await putPart(grant.upload, bytes, signal);
}

/** Finish an upload: every part stored and the content recognised (only then can it be posted). */
async function completeUpload(mediaId: string, purpose: MediaPurpose = "STATUS"): Promise<MediaView> {
  return mapMedia(await call<Wire>("POST", `${mediaBase(purpose)}/${id(mediaId)}/complete`, {}), purpose)!;
}

/** Upload a file in parts (resumable, parallel, retried); `onProgress` gets 0–1. */
async function upload(file: Blob, purpose: MediaPurpose, kind: MediaKind, durationMs: number | null, onProgress?: (p: number) => void, options: UploadOptions = {}): Promise<MediaView> {
  const { signal } = options;
  const contentType = (file.type || (kind === "AUDIO" ? "audio/webm" : kind === "VIDEO" ? "video/mp4" : "image/jpeg")).split(";")[0];
  let media: MediaView;
  if (options.mediaId) {
    media = await uploadState(options.mediaId, purpose);
  } else {
    media = mapMedia(await call<Wire>("POST", mediaBase(purpose), { kind, contentType, sizeBytes: file.size, durationMs: durationMs === null ? undefined : Math.round(durationMs) }), purpose)!;
    options.onStarted?.(media);
  }
  if (media.complete) {
    onProgress?.(1);
    return media;
  }
  const stored = new Set(media.storedParts ?? []);
  const todo = Array.from({ length: media.chunkCount }, (_, i) => i).filter((i) => !stored.has(i));
  let done = media.chunkCount - todo.length;
  onProgress?.(done / media.chunkCount);
  const onAbort = () => { void cancelUpload(media.mediaId, purpose).catch(() => {}); };
  signal?.addEventListener("abort", onAbort, { once: true });
  try {
    const worker = async () => {
      for (let part = todo.shift(); part !== undefined; part = todo.shift()) {
        const bytes = await file.slice(part * media.chunkBytes, Math.min(file.size, (part + 1) * media.chunkBytes)).arrayBuffer();
        const sha256 = await sha256Hex(bytes);
        const p = part;
        await retrying(signal, async () => {
          const grant = await call<{ stored: boolean; upload: { url: string; headers: { name: string; value: string }[] } | null }>(
            "POST", `${mediaBase(purpose)}/${id(media.mediaId)}/parts/${p}`, { sha256 });
          if (!grant.stored && grant.upload) await putPart(grant.upload, bytes, signal);
        });
        done += 1;
        onProgress?.(done / media.chunkCount);
      }
    };
    const workers = Math.max(1, Math.min(options.concurrency ?? UPLOAD_CONCURRENCY, 6));
    await Promise.all(Array.from({ length: workers }, worker));
    throwIfAborted(signal);
    return mapMedia(await retrying(signal, () => call<Wire>("POST", `${mediaBase(purpose)}/${id(media.mediaId)}/complete`, {})), purpose)!;
  } finally {
    signal?.removeEventListener("abort", onAbort);
  }
}

const blobs = new Map<string, Promise<string>>();

/** An object URL for a photo / video / voice note (downloaded once per page; the server authorizes every read). */
export function mediaUrl(media: MediaView): Promise<string> {
  let hit = blobs.get(media.mediaId);
  if (!hit) {
    hit = (async () => {
      const grant = await call<{ contentType: string; parts: { part: number; url: string }[] }>("POST", `${mediaBase(media.purpose ?? "STATUS")}/${id(media.mediaId)}/download-grants`, {});
      const ordered = [...grant.parts].sort((a, b) => a.part - b.part);
      const parts = await Promise.all(ordered.map(async (p) => {
        const r = await fetch(p.url, { credentials: "omit", redirect: "error" });
        if (!r.ok) throw new MessagingApiError(r.status, "DOWNLOAD_FAILED", null);
        return r.arrayBuffer();
      }));
      return URL.createObjectURL(new Blob(parts, { type: grant.contentType || media.contentType }));
    })();
    hit.catch(() => blobs.delete(media.mediaId));
    blobs.set(media.mediaId, hit);
  }
  return hit;
}

// ---------------------------------------------------------------------------------- API

export const socialService = {
  upload,
  uploadState,
  cancelUpload,
  startUpload,
  uploadPart,
  completeUpload,
  feed: async (): Promise<StatusFeed> => {
    const f = await call<Wire>("GET", `${S}/feed`);
    return { mine: (f.mine as Wire[]).map(mapStatus), contacts: await mapAuthors(f.contacts ?? []), muted: await mapAuthors(f.muted ?? []) };
  },
  /**
   * Publish a status. Idempotent per item: pass the same `idempotencyKey` on every retry
   * (within 24 h) and the original status comes back instead of a duplicate. Only the
   * caller's COMPLETE media can be posted.
   */
  postStatus: async (body: CreateStatus, idempotencyKey: string = uuid()) => mapStatus(await call<Wire>("POST", S, body, idempotencyKey)),
  getStatus: async (statusId: string) => mapStatus(await call<Wire>("GET", `${S}/${id(statusId)}`)),
  /** The caller's live statuses with audience, viewers (timestamps, reactions) and sharing. */
  myStatuses: async () => {
    const r = await call<Wire>("GET", `${S}/mine`);
    const posts = (r.posts as Wire[]) ?? [];
    await people(posts.flatMap((p) => (Array.isArray(p.viewers) ? p.viewers.map((v: Wire) => v.principalId as string) : [])));
    return { readReceipts: !!r.readReceipts, posts: posts.map(mapStatus) };
  },
  viewStatus: async (postId: string, liked?: boolean) => { await call("POST", `${S}/${id(postId)}/view`, liked === undefined ? {} : { liked }); },
  reactToStatus: async (postId: string, emoji: string | null) =>
    mapStatus(emoji ? await call<Wire>("PUT", `${S}/${id(postId)}/reaction`, { emoji }) : await call<Wire>("DELETE", `${S}/${id(postId)}/reaction`)),
  viewers: async (postId: string): Promise<StatusViewer[]> => {
    const r = await call<Wire>("GET", `${S}/${id(postId)}/viewers`);
    await people((r.viewers as Wire[]).map((v) => v.principalId));
    return (r.viewers as Wire[]).map(mapViewer);
  },
  deleteStatus: async (postId: string) => { await call("DELETE", `${S}/${id(postId)}`); },
  deleteStatuses: async (statusIds: string[]) => (await call<{ deleted: number }>("POST", `${S}/delete`, { statusIds })).deleted,
  /** Private reply: a message in the caller's direct chat with the author. */
  replyToStatus: (postId: string, text: string, clientMessageId: string = uuid()) =>
    call<{ statusId: string; conversationId: string; messageId: string }>("POST", `${S}/${id(postId)}/replies`, { text, clientMessageId }),
  forwardStatus: (postId: string, principalIds: string[]) =>
    call<{ statusId: string; forwarded: number; skipped: number }>("POST", `${S}/${id(postId)}/forwards`, { principalIds }),
  reshareStatus: async (postId: string, body: { caption?: string; audience?: AudienceChoice } = {}, idempotencyKey: string = uuid()) =>
    mapStatus(await call<Wire>("POST", `${S}/${id(postId)}/reshares`, body, idempotencyKey)),
  /** Rings for people on screen: only those whose statuses the caller may see come back. */
  rings: async (principalIds: string[]) => (await call<{ rings: StatusRing[] }>("POST", `${S}/rings`, { principalIds: principalIds.slice(0, 200) })).rings,
  getAudience: () => call<AudiencePreference>("GET", `${S}/audience`),
  putAudience: (body: { mode: AudienceMode; principalIds?: string[]; listId?: string; allowSharing?: boolean }) => call<AudiencePreference>("PUT", `${S}/audience`, body),
  /** The audience picker: mutual, unblocked contacts (named through the existing names API). */
  eligibleContacts: async (): Promise<PersonView[]> => {
    const r = await call<{ contacts: { principalId: string }[] }>("GET", `${S}/eligible-contacts`);
    const who = await people(r.contacts.map((c) => c.principalId));
    return r.contacts.map((c) => who.get(c.principalId)!);
  },
  closeFriends: async () => (await call<{ memberPrincipalIds: string[] }>("GET", `${S}/close-friends`)).memberPrincipalIds,
  setCloseFriends: async (memberPrincipalIds: string[]) => (await call<{ memberPrincipalIds: string[] }>("PUT", `${S}/close-friends`, { memberPrincipalIds })).memberPrincipalIds,
  audienceLists: async () => (await call<{ lists: AudienceList[] }>("GET", `${S}/audience-lists`)).lists,
  createAudienceList: (name: string, memberPrincipalIds: string[]) => call<AudienceList>("POST", `${S}/audience-lists`, { name, memberPrincipalIds }),
  updateAudienceList: (listId: string, body: { name?: string; memberPrincipalIds?: string[] }) => call<AudienceList>("PATCH", `${S}/audience-lists/${id(listId)}`, body),
  deleteAudienceList: async (listId: string) => { await call("DELETE", `${S}/audience-lists/${id(listId)}`); },
  mutes: async () => (await call<{ mutedPrincipalIds: string[] }>("GET", `${S}/mutes`)).mutedPrincipalIds,
  mute: async (principalId: string, on: boolean) => { await call(on ? "PUT" : "DELETE", `${S}/mutes/${id(principalId)}`); },
  directory: async (q: string) => mapChannels((await call<Wire>("GET", q.trim() ? `${CH}?q=${encodeURIComponent(q.trim().slice(0, 64))}` : CH)).channels),
  following: async () => mapChannels((await call<Wire>("GET", `${CH}/following`)).channels),
  channel: async (channelId: string) => (await mapChannels([await call<Wire>("GET", `${CH}/${id(channelId)}`)]))[0],
  createChannel: async (name: string, description: string) => (await mapChannels([await call<Wire>("POST", CH, { name, description })]))[0],
  updateChannel: async (channelId: string, body: { name?: string; description?: string }) => (await mapChannels([await call<Wire>("PATCH", `${CH}/${id(channelId)}`, body)]))[0],
  deleteChannel: async (channelId: string) => { await call("DELETE", `${CH}/${id(channelId)}`); },
  follow: async (channelId: string, on: boolean) => (await mapChannels([await call<Wire>(on ? "PUT" : "DELETE", `${CH}/${id(channelId)}/follow`)]))[0],
  posts: async (channelId: string, before?: string) =>
    ((await call<Wire>("GET", `${CH}/${id(channelId)}/posts${before ? `?before=${encodeURIComponent(before)}` : ""}`)).posts as Wire[]).map(mapPost),
  post: async (channelId: string, body: { text?: string; mediaId?: string }, idempotencyKey: string = uuid()) =>
    mapPost(await call<Wire>("POST", `${CH}/${id(channelId)}/posts`, body, idempotencyKey)),
  deletePost: async (channelId: string, postId: string) => { await call("DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}`); },
  react: async (channelId: string, postId: string, emoji: string | null) =>
    mapPost(emoji ? await call<Wire>("PUT", `${CH}/${id(channelId)}/posts/${id(postId)}/reaction`, { emoji })
      : await call<Wire>("DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}/reaction`)),
};

/** The kind of media a picked file is, or null. */
export function kindOf(file: File): MediaKind | null {
  if (file.type.startsWith("image/")) return "IMAGE";
  if (file.type.startsWith("video/")) return "VIDEO";
  if (file.type.startsWith("audio/")) return "AUDIO";
  return null;
}

/** A video or audio file's duration in ms (from its metadata). */
export function durationOf(file: Blob): Promise<number> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const el = document.createElement(file.type.startsWith("audio/") ? "audio" : "video");
    el.preload = "metadata";
    el.onloadedmetadata = () => {
      const d = el.duration;
      URL.revokeObjectURL(url);
      if (Number.isFinite(d) && d > 0) resolve(Math.round(d * 1000));
      else reject(new Error("Unknown duration"));
    };
    el.onerror = () => { URL.revokeObjectURL(url); reject(new Error("Unreadable file")); };
    el.src = url;
  });
}

/** Consecutive ≤ 60 s segments of a video of `durationMs` (5 min → 5 statuses). */
export function segments(durationMs: number): { startMs: number; endMs: number }[] {
  const out: { startMs: number; endMs: number }[] = [];
  for (let start = 0; start < durationMs; start += SEGMENT_MS) {
    const end = Math.min(durationMs, start + SEGMENT_MS);
    if (end - start >= 500 || out.length === 0) out.push({ startMs: start, endMs: end });
  }
  return out;
}
