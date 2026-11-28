/**
 * TRANSITIONAL (one release): the previous Status / Channels API on the Meeting Service
 * (`/api/v2/social/*`), used only while the backend still runs the release before #193.
 * TODO(#193 / ADR 0069 "Rollout"): delete after #193 is deployed (see social-backend.ts).
 *
 * Answers are mapped to the current screen types. Media is marked `source: "legacy"` so it is
 * read back from the same API.
 */
import apiV2 from "./api";
import type { Channel, ChannelPost, CreateStatus, MediaKind, MediaPurpose, MediaView, PersonView, StatusFeed, StatusPost, StatusViewer } from "./social.service";

// Wire shapes of the previous API.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Wire = any;

const id = (s: string) => encodeURIComponent(s);
const UPLOAD_PARALLELISM = 4;

function media(m: Wire, purpose: MediaPurpose): MediaView | null {
  return m ? { ...m, purpose, source: "legacy" } : null;
}

function legacyPerson(p: Wire): PersonView {
  return { userId: p?.principalId ?? p?.userId ?? "", name: p?.name ?? "Contact", avatarUrl: p?.avatarUrl ?? null, principalId: p?.principalId ?? null };
}

function status(p: Wire, author: string | null): StatusPost {
  return { ...p, statusId: p.postId, authorPrincipalId: author ?? "", media: media(p.media, "STATUS") };
}

function post(p: Wire): ChannelPost {
  return { ...p, media: media(p.media, "CHANNEL") };
}

function channel(c: Wire): Channel {
  return { ...c, owner: legacyPerson(c.owner), lastPost: c.lastPost ? post(c.lastPost) : null };
}

/** The previous API takes only these fields (unknown ones are refused). */
function legacyBody(b: CreateStatus): Record<string, unknown> {
  switch (b.kind) {
    case "TEXT": return { kind: "TEXT", text: b.text, background: b.background ?? b.backgroundGradient?.from, font: b.font };
    case "VIDEO": return { kind: "VIDEO", mediaId: b.mediaId, startMs: b.startMs, endMs: b.endMs, caption: b.caption };
    default: return { kind: b.kind, mediaId: b.mediaId, caption: b.caption };
  }
}

export async function startUpload(purpose: MediaPurpose, kind: MediaKind, contentType: string, sizeBytes: number, durationMs: number | null): Promise<MediaView> {
  return media((await apiV2.post("/social/media", { purpose, kind, contentType, sizeBytes, durationMs: durationMs === null ? undefined : Math.round(durationMs) })).data, purpose)!;
}

export async function uploadPart(mediaId: string, index: number, body: Blob, signal?: AbortSignal): Promise<void> {
  const bytes = new Uint8Array(await body.arrayBuffer());
  await apiV2.put(`/social/media/${id(mediaId)}/chunks/${index}`, bytes, { signal, headers: { "Content-Type": "application/octet-stream" }, transformRequest: [(d) => d] });
}

export async function completeUpload(mediaId: string, purpose: MediaPurpose): Promise<MediaView> {
  return media((await apiV2.post(`/social/media/${id(mediaId)}/complete`, {})).data, purpose)!;
}

/** Upload in 512 KiB pieces (not resumable across reloads: the previous API cannot list them). */
export async function upload(file: Blob, purpose: MediaPurpose, kind: MediaKind, durationMs: number | null, onProgress?: (p: number) => void, signal?: AbortSignal): Promise<MediaView> {
  const contentType = (file.type || (kind === "AUDIO" ? "audio/webm" : kind === "VIDEO" ? "video/mp4" : "image/jpeg")).split(";")[0];
  const started = await startUpload(purpose, kind, contentType, file.size, durationMs);
  let next = 0;
  let done = 0;
  let failed = false;
  const putPiece = async (seq: number) => {
    const piece = file.slice(seq * started.chunkBytes, Math.min(file.size, (seq + 1) * started.chunkBytes));
    for (let attempt = 0; ; attempt++) {
      try {
        await uploadPart(started.mediaId, seq, piece, signal);
        return;
      } catch (e) {
        if (attempt >= 3 || failed || signal?.aborted) throw e;
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
  return completeUpload(started.mediaId, purpose);
}

/** The bytes of legacy media (pieces read through the API). */
export async function mediaBlob(m: MediaView): Promise<Blob> {
  const parts: ArrayBuffer[] = [];
  for (let seq = 0; seq < m.chunkCount; seq++) {
    parts.push((await apiV2.get<ArrayBuffer>(`/social/media/${id(m.mediaId)}/chunks/${seq}`, { responseType: "arraybuffer" })).data);
  }
  return new Blob(parts, { type: m.contentType });
}

export const legacySocial = {
  feed: async (): Promise<StatusFeed> => {
    const f = (await apiV2.get("/social/status/feed")).data as Wire;
    return {
      mine: (f.mine as Wire[]).map((p) => status(p, null)),
      contacts: (f.contacts as Wire[]).map((a) => ({ ...a, person: legacyPerson(a.person), posts: (a.posts as Wire[]).map((p) => status(p, a.person?.principalId ?? null)) })),
      muted: [],
    };
  },
  postStatus: async (body: CreateStatus) => status((await apiV2.post("/social/status", legacyBody(body))).data, null),
  viewStatus: async (postId: string, liked?: boolean) => { await apiV2.post(`/social/status/${id(postId)}/view`, liked === undefined ? {} : { liked }); },
  viewers: async (postId: string): Promise<StatusViewer[]> =>
    ((await apiV2.get(`/social/status/${id(postId)}/viewers`)).data as Wire[]).map((v) => ({ ...v, person: legacyPerson(v.person) })),
  deleteStatus: async (postId: string) => { await apiV2.delete(`/social/status/${id(postId)}`); },
  directory: async (q: string) => ((await apiV2.get("/social/channels", { params: q.trim() ? { q: q.trim() } : {} })).data as Wire[]).map(channel),
  following: async () => ((await apiV2.get("/social/channels/following")).data as Wire[]).map(channel),
  channel: async (channelId: string) => channel((await apiV2.get(`/social/channels/${id(channelId)}`)).data),
  createChannel: async (name: string, description: string) => channel((await apiV2.post("/social/channels", { name, description })).data),
  updateChannel: async (channelId: string, body: { name?: string; description?: string }) => channel((await apiV2.patch(`/social/channels/${id(channelId)}`, body)).data),
  deleteChannel: async (channelId: string) => { await apiV2.delete(`/social/channels/${id(channelId)}`); },
  follow: async (channelId: string, on: boolean) =>
    channel(on ? (await apiV2.put(`/social/channels/${id(channelId)}/follow`)).data : (await apiV2.delete(`/social/channels/${id(channelId)}/follow`)).data),
  posts: async (channelId: string, before?: string) =>
    ((await apiV2.get(`/social/channels/${id(channelId)}/posts`, { params: before ? { before } : {} })).data as Wire[]).map(post),
  post: async (channelId: string, body: { text?: string; mediaId?: string }) => post((await apiV2.post(`/social/channels/${id(channelId)}/posts`, body)).data),
  deletePost: async (channelId: string, postId: string) => { await apiV2.delete(`/social/channels/${id(channelId)}/posts/${id(postId)}`); },
  react: async (channelId: string, postId: string, emoji: string | null) =>
    post(emoji ? (await apiV2.put(`/social/channels/${id(channelId)}/posts/${id(postId)}/reaction`, { emoji })).data
      : (await apiV2.delete(`/social/channels/${id(channelId)}/posts/${id(postId)}/reaction`)).data),
};
