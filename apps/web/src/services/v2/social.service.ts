/**
 * Updates (WhatsApp-style): status (24 hours, your contacts) and channels.
 * Media goes up and comes back in 512 KiB pieces through the normal API.
 */
import apiV2 from "./api";

export type MediaKind = "IMAGE" | "VIDEO" | "AUDIO";
export type PostKind = "TEXT" | MediaKind;

export interface MediaView {
  mediaId: string;
  kind: MediaKind;
  contentType: string;
  sizeBytes: number;
  chunkBytes: number;
  chunkCount: number;
  durationMs: number | null;
  complete: boolean;
}

export interface PersonView {
  userId: string;
  name: string;
  avatarUrl: string | null;
  principalId: string | null;
}

export interface StatusPost {
  postId: string;
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
}

export interface StatusAuthor {
  person: PersonView;
  posts: StatusPost[];
  allViewed: boolean;
  lastPostedAt: string;
}

export interface StatusFeed {
  mine: StatusPost[];
  contacts: StatusAuthor[];
}

export interface StatusViewer {
  person: PersonView;
  viewedAt: string;
  liked: boolean;
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

export type CreateStatus =
  | { kind: "TEXT"; text: string; background?: string; font?: number }
  | { kind: "IMAGE" | "AUDIO"; mediaId: string; caption?: string }
  | { kind: "VIDEO"; mediaId: string; startMs: number; endMs: number; caption?: string };

export const SEGMENT_MS = 60_000;
export const LIMITS: Record<MediaKind, number> = { IMAGE: 8 * 1024 * 1024, AUDIO: 8 * 1024 * 1024, VIDEO: 64 * 1024 * 1024 };

const id = (s: string) => encodeURIComponent(s);
/** Pieces of one upload in flight at once. */
const UPLOAD_PARALLELISM = 4;

/** Upload a file in pieces; `onProgress` gets 0–1. */
async function upload(file: Blob, purpose: "STATUS" | "CHANNEL", kind: MediaKind, durationMs: number | null, onProgress?: (p: number) => void): Promise<MediaView> {
  const contentType = (file.type || (kind === "AUDIO" ? "audio/webm" : kind === "VIDEO" ? "video/mp4" : "image/jpeg")).split(";")[0];
  const started = (await apiV2.post<MediaView>("/social/media", { purpose, kind, contentType, sizeBytes: file.size, durationMs: durationMs === null ? undefined : Math.round(durationMs) })).data;
  // Pieces go up UPLOAD_PARALLELISM at a time (the server stores each by its number), each
  // retried a few times; the first piece that keeps failing stops the others.
  let next = 0;
  let done = 0;
  let failed = false;
  const putPiece = async (seq: number) => {
    const piece = file.slice(seq * started.chunkBytes, Math.min(file.size, (seq + 1) * started.chunkBytes));
    const body = new Uint8Array(await piece.arrayBuffer());
    for (let attempt = 0; ; attempt++) {
      try {
        await apiV2.put(`/social/media/${id(started.mediaId)}/chunks/${seq}`, body, { headers: { "Content-Type": "application/octet-stream" }, transformRequest: [(d) => d] });
        return;
      } catch (e) {
        if (attempt >= 3 || failed) throw e;
        await new Promise((r) => setTimeout(r, 800 * (attempt + 1)));
      }
    }
  };
  const worker = async () => {
    while (!failed && next < started.chunkCount) {
      const seq = next++;
      try {
        await putPiece(seq);
      } catch (e) {
        failed = true;
        throw e;
      }
      onProgress?.(++done / started.chunkCount);
    }
  };
  await Promise.all(Array.from({ length: Math.min(UPLOAD_PARALLELISM, started.chunkCount) }, worker));
  return (await apiV2.post<MediaView>(`/social/media/${id(started.mediaId)}/complete`, {})).data;
}

const blobs = new Map<string, Promise<string>>();

/** An object URL for a photo / video / voice note (downloaded once per page). */
export function mediaUrl(media: MediaView): Promise<string> {
  let hit = blobs.get(media.mediaId);
  if (!hit) {
    hit = (async () => {
      const parts: ArrayBuffer[] = [];
      for (let seq = 0; seq < media.chunkCount; seq++) {
        const r = await apiV2.get<ArrayBuffer>(`/social/media/${id(media.mediaId)}/chunks/${seq}`, { responseType: "arraybuffer" });
        parts.push(r.data);
      }
      return URL.createObjectURL(new Blob(parts, { type: media.contentType }));
    })();
    hit.catch(() => blobs.delete(media.mediaId));
    blobs.set(media.mediaId, hit);
  }
  return hit;
}

export const socialService = {
  upload,
  feed: async () => (await apiV2.get<StatusFeed>("/social/status/feed")).data,
  postStatus: async (body: CreateStatus) => (await apiV2.post<StatusPost>("/social/status", body)).data,
  viewStatus: async (postId: string, liked?: boolean) => { await apiV2.post(`/social/status/${id(postId)}/view`, liked === undefined ? {} : { liked }); },
  viewers: async (postId: string) => (await apiV2.get<StatusViewer[]>(`/social/status/${id(postId)}/viewers`)).data,
  deleteStatus: async (postId: string) => { await apiV2.delete(`/social/status/${id(postId)}`); },
  directory: async (q: string) => (await apiV2.get<Channel[]>("/social/channels", { params: q.trim() ? { q: q.trim() } : {} })).data,
  following: async () => (await apiV2.get<Channel[]>("/social/channels/following")).data,
  channel: async (channelId: string) => (await apiV2.get<Channel>(`/social/channels/${id(channelId)}`)).data,
  createChannel: async (name: string, description: string) => (await apiV2.post<Channel>("/social/channels", { name, description })).data,
  updateChannel: async (channelId: string, body: { name?: string; description?: string }) => (await apiV2.patch<Channel>(`/social/channels/${id(channelId)}`, body)).data,
  deleteChannel: async (channelId: string) => { await apiV2.delete(`/social/channels/${id(channelId)}`); },
  follow: async (channelId: string, on: boolean) => (on ? (await apiV2.put<Channel>(`/social/channels/${id(channelId)}/follow`)).data : (await apiV2.delete<Channel>(`/social/channels/${id(channelId)}/follow`)).data),
  posts: async (channelId: string, before?: string) => (await apiV2.get<ChannelPost[]>(`/social/channels/${id(channelId)}/posts`, { params: before ? { before } : {} })).data,
  post: async (channelId: string, body: { text?: string; mediaId?: string }) => (await apiV2.post<ChannelPost>(`/social/channels/${id(channelId)}/posts`, body)).data,
  deletePost: async (channelId: string, postId: string) => { await apiV2.delete(`/social/channels/${id(channelId)}/posts/${id(postId)}`); },
  react: async (channelId: string, postId: string, emoji: string | null) =>
    emoji ? (await apiV2.put<ChannelPost>(`/social/channels/${id(channelId)}/posts/${id(postId)}/reaction`, { emoji })).data
      : (await apiV2.delete<ChannelPost>(`/social/channels/${id(channelId)}/posts/${id(postId)}/reaction`)).data,
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
