/**
 * Channels, complete (ADR 0072): Messaging's `/v1/messaging/channels/**` through the Meeting
 * Service relay (`/api/v2/messaging/relay/...`), as the signed-in user's own principal.
 * Every rule (private channels, admin permissions, one vote per follower, verification and
 * moderation decided only by CF Meet) is enforced by the server; nothing here filters.
 */
import { messagingService, relay } from "./messaging.service";
import { MessagingApiError } from "@/lib/messaging/errors";
import { socialService, type MediaView } from "./social.service";

type Method = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
const CH = "v1/messaging/channels";
const id = (s: string) => encodeURIComponent(s);

/** Errors keep the `response.data.error` shape the screens read (`errorText`). */
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

export type Visibility = "PUBLIC" | "PRIVATE";
export type ModerationState = "ACTIVE" | "RESTRICTED" | "SUSPENDED" | "BANNED";
export type Role = "OWNER" | "ADMIN" | "FOLLOWER" | "NONE";
export type PostKind = "TEXT" | "IMAGE" | "VIDEO" | "AUDIO" | "VOICE" | "DOCUMENT" | "GIF" | "STICKER" | "POLL";
export type Sort = "POPULAR" | "TRENDING" | "NEW" | "VERIFIED";

export interface WireMedia {
  mediaId: string; kind: "IMAGE" | "VIDEO" | "AUDIO" | "DOCUMENT"; contentType: string; sizeBytes: number;
  partBytes: number; partCount: number; durationMs: number | null; complete: boolean; fileName?: string;
}
export interface LinkPreview { url: string; title: string | null; description: string | null; siteName: string | null }
export interface PollOption { index: number; text: string; votes: number }
export interface Poll { question: string; multiple: boolean; options: PollOption[]; totalVoters: number; myVotes: number[]; closesAt: string | null; closed: boolean }
export interface Post {
  postId: string; channelId: string; state: "PUBLISHED" | "SCHEDULED"; kind: PostKind; text: string | null;
  media: WireMedia | null; linkPreview: LinkPreview | null; poll: Poll | null; createdAt: string; publishAt?: string;
  edited: boolean; editedAt?: string | null; pinned: boolean; pinnedAt?: string | null;
  reactions: { emoji: string; count: number }[]; myReaction: string | null; starred: boolean; viewCount: number | null; mine?: boolean;
}
export interface Permissions { post: boolean; edit: boolean; manageAdmins: boolean }
export interface Channel {
  channelId: string; name: string; description: string | null; handle: string | null; category: string | null;
  language: string | null; country: string | null; visibility: Visibility; avatar: WireMedia | null; verified: boolean;
  verifiedAt: string | null; moderationState: ModerationState; ownerPrincipalId: string; followerCount: number;
  following: boolean; muted: boolean; isOwner: boolean; myRole: Role; permissions: Permissions; unreadCount: number;
  lastPost: Post | null; createdAt: string; pinnedPosts?: Post[];
}
export interface Suggestion { channelId: string; name: string; handle: string | null; verified: boolean; followerCount: number; following?: boolean; avatar: WireMedia | null }
export interface Lookup { handle: string; valid: boolean; available: boolean; reason: "INVALID" | "RESERVED" | "TAKEN" | null; channel: Channel | null }
export interface Admin { principalId: string; canPost: boolean; canEdit: boolean; canManageAdmins: boolean; addedAt: string; name?: string }
export interface Admins { channelId: string; ownerPrincipalId: string; admins: Admin[]; ownerName?: string }
export interface Invite { inviteId: string; channelId: string; createdAt: string; expiresAt: string; maxUses: number | null; useCount: number; revoked: boolean; active: boolean; token?: string }
export interface InvitePreview { channelId: string; name: string; description: string | null; handle: string | null; category: string | null; visibility: Visibility; verified: boolean; followerCount: number; following: boolean; avatar: WireMedia | null }
export interface Application { applicationId: string; basis: string; state: "PENDING" | "APPROVED" | "REJECTED" | "REVOKED"; decisionReason: string | null; decidedAt: string | null; createdAt: string }
export interface Verification { channelId: string; verified: boolean; verifiedAt: string | null; applications: Application[] }
export interface ModerationAction { actionId: string; action: string; resultingState: string | null; reason: string; postId: string | null; createdAt: string }
export interface Moderation { channelId: string; moderationState: ModerationState; verified: boolean; actions: ModerationAction[] }
export interface AuditEntry { entryId: string; action: string; actorPrincipalId: string; targetPrincipalId: string | null; targetId: string | null; occurredAt: string }
export interface AnalyticsDay { date: string; follows: number; unfollows: number; followers: number | null; views: number; uniqueViewers: number; reactions: number; pollVoters: number; forwards: number }
export interface Analytics {
  channelId: string; from: string; to: string; statsSince: string;
  followers: { current: number; atStart: number | null; follows: number; unfollows: number; net: number };
  posts: { published: number; views: number; uniqueViewers: number; reactions: number; pollVoters: number; forwards: number };
  engagementRate: number | null; series: AnalyticsDay[];
  topPosts: { postId: string; kind: string; excerpt: string | null; createdAt: string; views: number; reactions: number; pollVoters: number; forwards: number }[];
}
export interface Starred { post: Post; channel: { channelId: string; name: string; handle: string | null; verified: boolean }; starredAt: string }

export interface ChannelFields {
  name?: string; description?: string; handle?: string; category?: string; language?: string; country?: string;
  visibility?: Visibility; avatarMediaId?: string | null;
}
export interface DiscoverQuery { q?: string; category?: string; country?: string; language?: string; sort?: Sort; cursor?: string; limit?: number }
export interface PostBody {
  text?: string; mediaId?: string; presentation?: "VOICE" | "GIF" | "STICKER";
  linkPreview?: { url: string; title?: string; description?: string; siteName?: string };
  poll?: { question: string; options: string[]; multiple?: boolean }; publishAt?: string;
}

/** A channel media view as the shared media reader expects it (`mediaUrl`). */
export function mediaView(m: WireMedia): MediaView {
  return {
    mediaId: m.mediaId, kind: m.kind as MediaView["kind"], contentType: m.contentType, sizeBytes: m.sizeBytes,
    chunkBytes: m.partBytes, chunkCount: m.partCount, durationMs: m.durationMs ?? null, complete: !!m.complete, purpose: "CHANNEL",
  };
}

function query(q: Record<string, string | number | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
}

async function named<T extends { principalId: string }>(list: T[]): Promise<(T & { name: string })[]> {
  const names = new Map<string, string>();
  try {
    const r = await messagingService.names(list.map((x) => x.principalId).slice(0, 256));
    for (const p of r.principals) names.set(p.principalId, p.alias || p.displayName);
  } catch {
    // Names are display-only; a lookup failure never fails the screen.
  }
  return list.map((x) => ({ ...x, name: names.get(x.principalId) ?? "Admin" }));
}

/** Upload a document (its file name is kept) or any other channel media, in parts. */
async function upload(file: File, kind: WireMedia["kind"], durationMs: number | null, onProgress?: (p: number) => void): Promise<WireMedia> {
  if (kind !== "DOCUMENT") {
    const m = await socialService.upload(file, "CHANNEL", kind, durationMs, onProgress);
    return { mediaId: m.mediaId, kind, contentType: m.contentType, sizeBytes: m.sizeBytes, partBytes: m.chunkBytes, partCount: m.chunkCount, durationMs: m.durationMs, complete: m.complete };
  }
  const contentType = (file.type || "application/octet-stream").split(";")[0];
  const started = await call<WireMedia>("POST", `${CH}/media`, { kind, contentType, sizeBytes: file.size, fileName: file.name.slice(-200) });
  for (let part = 0; part < started.partCount; part++) {
    await socialService.uploadPart(started.mediaId, part, file.slice(part * started.partBytes, (part + 1) * started.partBytes), "CHANNEL");
    onProgress?.((part + 1) / started.partCount);
  }
  return call<WireMedia>("POST", `${CH}/media/${id(started.mediaId)}/complete`, {});
}

export const channelsService = {
  upload,
  discover: (q: DiscoverQuery) => call<{ channels: Channel[]; nextCursor: string | null }>("GET", `${CH}${query({ ...q, q: q.q?.trim().slice(0, 64) })}`),
  suggestions: (q?: string) => call<{ suggestions: Suggestion[] }>("GET", `${CH}/suggestions${query({ q: q?.trim().slice(0, 64) })}`),
  lookup: (handle: string) => call<Lookup>("GET", `${CH}/lookup${query({ handle: handle.slice(0, 64) })}`),
  following: () => call<{ channels: Channel[] }>("GET", `${CH}/following`),
  get: (channelId: string) => call<Channel>("GET", `${CH}/${id(channelId)}`),
  create: (body: ChannelFields) => call<Channel>("POST", CH, body),
  update: (channelId: string, body: ChannelFields) => call<Channel>("PATCH", `${CH}/${id(channelId)}`, body),
  remove: (channelId: string) => call<{ deleted: boolean }>("DELETE", `${CH}/${id(channelId)}`),
  follow: (channelId: string, on: boolean) => call<Channel | { channelId: string; following: false }>(on ? "PUT" : "DELETE", `${CH}/${id(channelId)}/follow`),
  mute: (channelId: string, on: boolean) => call<Channel>(on ? "PUT" : "DELETE", `${CH}/${id(channelId)}/mute`),
  posts: (channelId: string, before?: string) => call<{ posts: Post[] }>("GET", `${CH}/${id(channelId)}/posts${query({ before })}`),
  post: (channelId: string, body: PostBody, idempotencyKey: string = crypto.randomUUID()) =>
    call<Post>("POST", `${CH}/${id(channelId)}/posts`, body, idempotencyKey),
  edit: (channelId: string, postId: string, body: { text?: string; linkPreview?: PostBody["linkPreview"] | null }) =>
    call<Post>("PATCH", `${CH}/${id(channelId)}/posts/${id(postId)}`, body),
  deletePost: (channelId: string, postId: string) => call<{ deleted: boolean }>("DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}`),
  react: (channelId: string, postId: string, emoji: string | null) =>
    emoji ? call<Post>("PUT", `${CH}/${id(channelId)}/posts/${id(postId)}/reaction`, { emoji }) : call<Post>("DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}/reaction`),
  pin: (channelId: string, postId: string, on: boolean) => call<Post>(on ? "PUT" : "DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}/pin`),
  vote: (channelId: string, postId: string, optionIndexes: number[] | null) =>
    optionIndexes ? call<Post>("PUT", `${CH}/${id(channelId)}/posts/${id(postId)}/vote`, { optionIndexes }) : call<Post>("DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}/vote`),
  star: (channelId: string, postId: string, on: boolean) => call<Post>(on ? "PUT" : "DELETE", `${CH}/${id(channelId)}/posts/${id(postId)}/star`),
  views: (channelId: string, postIds: string[]) => call<{ recorded: number }>("POST", `${CH}/${id(channelId)}/posts/views`, { postIds: postIds.slice(0, 100) }),
  forward: (channelId: string, postId: string, principalIds: string[]) =>
    call<{ forwarded: number; skipped: number }>("POST", `${CH}/${id(channelId)}/posts/${id(postId)}/forwards`, { principalIds: principalIds.slice(0, 20) }),
  starred: (cursor?: string) => call<{ items: Starred[]; nextCursor: string | null }>("GET", `${CH}/starred${query({ cursor })}`),
  scheduled: (channelId: string) => call<{ posts: Post[] }>("GET", `${CH}/${id(channelId)}/scheduled`),
  cancelScheduled: (channelId: string, postId: string) => call<{ deleted: boolean }>("DELETE", `${CH}/${id(channelId)}/scheduled/${id(postId)}`),
  admins: async (channelId: string): Promise<Admins> => {
    const r = await call<Admins>("GET", `${CH}/${id(channelId)}/admins`);
    const [owner, ...admins] = await named([{ principalId: r.ownerPrincipalId }, ...r.admins] as Admin[]);
    return { ...r, ownerName: owner.name, admins: admins as Admin[] };
  },
  putAdmin: (channelId: string, principalId: string, perms: { canPost: boolean; canEdit: boolean; canManageAdmins: boolean }) =>
    call<Admins>("PUT", `${CH}/${id(channelId)}/admins/${id(principalId)}`, perms),
  removeAdmin: (channelId: string, principalId: string) => call<unknown>("DELETE", `${CH}/${id(channelId)}/admins/${id(principalId)}`),
  transfer: (channelId: string, principalId: string) => call<Channel>("POST", `${CH}/${id(channelId)}/owner`, { principalId }),
  invites: (channelId: string) => call<{ invites: Invite[] }>("GET", `${CH}/${id(channelId)}/invites`),
  createInvite: (channelId: string, body: { expiresInSeconds?: number; maxUses?: number }) => call<Invite>("POST", `${CH}/${id(channelId)}/invites`, body),
  revokeInvite: (channelId: string, inviteId: string) => call<Invite>("DELETE", `${CH}/${id(channelId)}/invites/${id(inviteId)}`),
  previewInvite: (token: string) => call<InvitePreview>("POST", `${CH}/invites/preview`, { token }),
  joinInvite: (token: string) => call<Channel>("POST", `${CH}/invites/join`, { token }),
  report: (channelId: string, body: { category: string; postId?: string; note?: string }) =>
    call<{ reportId: string; status: string }>("POST", `${CH}/${id(channelId)}/reports`, body),
  verification: (channelId: string) => call<Verification>("GET", `${CH}/${id(channelId)}/verification`),
  applyVerification: (channelId: string, basis: string) => call<Application>("POST", `${CH}/${id(channelId)}/verification`, { basis }),
  moderation: (channelId: string) => call<Moderation>("GET", `${CH}/${id(channelId)}/moderation`),
  audit: (channelId: string, cursor?: string) => call<{ items: AuditEntry[]; nextCursor: string | null }>("GET", `${CH}/${id(channelId)}/audit${query({ cursor })}`),
  analytics: (channelId: string, from?: string, to?: string) => call<Analytics>("GET", `${CH}/${id(channelId)}/analytics${query({ from, to })}`),
};
