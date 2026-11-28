/**
 * Communities (ADR 0073, Messaging): types of the `/v1/messaging/communities` API and the pure
 * rules the screens use. The server enforces every permission; these helpers only decide
 * what to show (an action the server would refuse is not offered).
 */

export type CommunityPrivacy = "OPEN" | "APPROVAL" | "INVITE_ONLY";
export type CommunityRole = "OWNER" | "ADMIN" | "MEMBER";
export type GroupAccess = "OPEN" | "RESTRICTED";
export type ChatListMode = "TOGETHER" | "SEPARATE";

export type Community = {
  communityId: string;
  name: string;
  description: string | null;
  rules: string | null;
  privacy: CommunityPrivacy;
  membersCanAddGroups: boolean;
  ownerPrincipalId: string;
  announcementsConversationId: string | null;
  pictureRef: string | null;
  memberCount: number;
  groupCount: number;
  myRole: CommunityRole;
  joinedAt: string;
  joinedConversationIds: string[];
  pendingRequestCount: number | null;
  version: number;
  createdAt: string;
  updatedAt: string;
};

export type CommunityGroup = {
  conversationId: string;
  title: string | null;
  description: string | null;
  avatarRef: string | null;
  securityProfile: string;
  access: GroupAccess;
  position: number;
  announcements: boolean;
  joined: boolean;
  memberCount: number | null;
  myRole: string | null;
  canManage: boolean;
  pendingRequest: boolean;
  linkedAt: string;
};

export type CommunityMember = { principalId: string; role: CommunityRole; joinedAt: string };
export type CommunityBan = { principalId: string; bannedAt: string; bannedByPrincipalId: string | null };

export type CommunityPreview = {
  communityId: string;
  name: string;
  description: string | null;
  rules: string | null;
  privacy: CommunityPrivacy;
  memberCount: number;
  groupCount: number;
  member: boolean;
  pendingRequest: boolean;
};

export type CommunityInvitation = {
  invitationId: string;
  communityId: string;
  principalId: string;
  invitedByPrincipalId: string | null;
  state: "PENDING" | "ACCEPTED" | "DECLINED" | "REVOKED" | "EXPIRED";
  createdAt: string;
  expiresAt: string;
  community?: CommunityPreview;
};

export type CommunityInviteLink = {
  inviteLinkId: string;
  communityId: string;
  maxUses: number | null;
  useCount: number;
  expiresAt: string;
  createdByPrincipalId: string | null;
  createdAt: string;
  revokedAt: string | null;
  status: "ACTIVE" | "EXPIRED" | "EXHAUSTED" | "REVOKED";
  /** Only in the creation response. */
  token?: string;
};

export type JoinRequest = {
  requestId: string;
  communityId: string;
  communityName: string | null;
  principalId: string;
  conversationId: string | null;
  groupTitle: string | null;
  state: "PENDING" | "APPROVED" | "REJECTED" | "CANCELLED";
  message: string | null;
  createdAt: string;
  decidedAt: string | null;
  decidedByPrincipalId: string | null;
};

export type JoinOutcome = {
  outcome: "JOINED" | "REQUESTED" | "ALREADY_MEMBER";
  community?: Community;
  group?: CommunityGroup;
  request?: JoinRequest;
};

export type Page<K extends string, T> = { [key in K]: T[] } & { nextCursor: string | null };

// ------------------------------------------------------------------------- validation

export const MAX_NAME = 100;
export const MAX_DESCRIPTION = 2048;
export const MAX_RULES = 4000;
export const MAX_GROUP_TITLE = 120;
export const MAX_GROUP_DESCRIPTION = 512;
export const MAX_REQUEST_MESSAGE = 500;
export const MAX_INVITEES = 50;

const CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/;

/** Trimmed text of 1..max characters (or empty when optional); null when not valid. */
export function cleanText(value: string, max: number, optional = false): string | null {
  const t = value.trim();
  if (!t) return optional ? "" : null;
  if ([...t].length > max || CONTROL.test(t)) return null;
  return t;
}

export type CommunityDraft = { name: string; description: string; rules: string; privacy: CommunityPrivacy; membersCanAddGroups: boolean };

/** The problem with a draft (null: it can be sent). */
export function draftError(d: CommunityDraft): string | null {
  if (cleanText(d.name, MAX_NAME) === null) return d.name.trim() ? `The name can be at most ${MAX_NAME} characters.` : "Give your community a name.";
  if (cleanText(d.description, MAX_DESCRIPTION, true) === null) return `The description can be at most ${MAX_DESCRIPTION} characters.`;
  if (cleanText(d.rules, MAX_RULES, true) === null) return `The rules can be at most ${MAX_RULES} characters.`;
  return null;
}

/** The creation body of a valid draft (empty optional texts are left out). */
export function createBody(d: CommunityDraft) {
  const description = cleanText(d.description, MAX_DESCRIPTION, true);
  const rules = cleanText(d.rules, MAX_RULES, true);
  return {
    name: cleanText(d.name, MAX_NAME)!,
    ...(description ? { description } : {}),
    ...(rules ? { rules } : {}),
    privacy: d.privacy,
    membersCanAddGroups: d.membersCanAddGroups,
  };
}

/** A settings change: only what differs from the community (null clears a text). */
export function patchBody(c: Community, d: CommunityDraft): Record<string, unknown> {
  const out: Record<string, unknown> = { expectedVersion: c.version };
  const name = cleanText(d.name, MAX_NAME);
  if (name && name !== c.name) out.name = name;
  const description = cleanText(d.description, MAX_DESCRIPTION, true);
  if (description !== null && (description || null) !== c.description) out.description = description || null;
  const rules = cleanText(d.rules, MAX_RULES, true);
  if (rules !== null && (rules || null) !== c.rules) out.rules = rules || null;
  if (d.privacy !== c.privacy) out.privacy = d.privacy;
  if (d.membersCanAddGroups !== c.membersCanAddGroups) out.membersCanAddGroups = d.membersCanAddGroups;
  return out;
}

export const draftOf = (c: Community): CommunityDraft => ({
  name: c.name, description: c.description ?? "", rules: c.rules ?? "", privacy: c.privacy, membersCanAddGroups: c.membersCanAddGroups,
});

export const PRIVACY_LABELS: Record<CommunityPrivacy, { label: string; detail: string }> = {
  OPEN: { label: "Open", detail: "Anyone with an invite link joins straight away. Any member can invite." },
  APPROVAL: { label: "Admin approval", detail: "Anyone can be invited, but admins approve people who join by link." },
  INVITE_ONLY: { label: "Invite only", detail: "Only admins invite people or create invite links." },
};

// ------------------------------------------------------------------------- permissions

export const isAdmin = (role: CommunityRole | null | undefined) => role === "OWNER" || role === "ADMIN";
export const canManageSettings = isAdmin;
/** Invitations and invite links: admins always; members unless the community is invite-only. */
export const canInvite = (privacy: CommunityPrivacy, role: CommunityRole | null | undefined) =>
  !!role && (isAdmin(role) || privacy !== "INVITE_ONLY");
export const canAddGroups = (c: Pick<Community, "myRole" | "membersCanAddGroups">) => isAdmin(c.myRole) || c.membersCanAddGroups;
/** The owner removes anyone but themselves; an admin removes members only. */
export function canRemove(actor: CommunityRole | null | undefined, target: CommunityRole): boolean {
  if (target === "OWNER") return false;
  if (actor === "OWNER") return true;
  return actor === "ADMIN" && target === "MEMBER";
}
export const canPromote = (actor: CommunityRole | null | undefined, target: CommunityRole) => isAdmin(actor) && target === "MEMBER";
/** Only the owner demotes an admin (an admin may step down themselves). */
export const canDemote = (actor: CommunityRole | null | undefined, target: CommunityRole, self = false) =>
  target === "ADMIN" && (actor === "OWNER" || (self && actor === "ADMIN"));
export const canTransfer = (actor: CommunityRole | null | undefined) => actor === "OWNER";
export const canDelete = canTransfer;
/** The owner hands the community over before leaving. */
export const canLeave = (actor: CommunityRole | null | undefined) => !!actor && actor !== "OWNER";

export const ROLE_LABELS: Record<CommunityRole, string> = { OWNER: "Owner", ADMIN: "Community admin", MEMBER: "Member" };

// ------------------------------------------------------------------------------ groups

/** Announcements first, then the community's own order. */
export function sortGroups<T extends Pick<CommunityGroup, "announcements" | "position" | "conversationId">>(groups: readonly T[]): T[] {
  return [...groups].sort((a, b) => Number(b.announcements) - Number(a.announcements) || a.position - b.position || a.conversationId.localeCompare(b.conversationId));
}

export function splitGroups<T extends Pick<CommunityGroup, "announcements" | "position" | "conversationId" | "joined">>(groups: readonly T[]) {
  const sorted = sortGroups(groups);
  return { joined: sorted.filter((g) => g.joined), joinable: sorted.filter((g) => !g.joined) };
}

/** The new order after moving one (non-announcement) group up or down; null when it cannot move. */
export function moveGroup(groups: readonly CommunityGroup[], conversationId: string, delta: -1 | 1): string[] | null {
  const ordered = sortGroups(groups).filter((g) => !g.announcements).map((g) => g.conversationId);
  const i = ordered.indexOf(conversationId);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= ordered.length) return null;
  [ordered[i], ordered[j]] = [ordered[j], ordered[i]];
  return ordered;
}

export const groupLabel = (g: Pick<CommunityGroup, "title" | "announcements">, fallback?: string | null) =>
  g.announcements ? "Announcements" : g.title?.trim() || fallback?.trim() || "Group";

// ------------------------------------------------------------------------- invite links

export function inviteLinkUrl(origin: string, token: string): string {
  return `${origin.replace(/\/+$/, "")}/dashboard/communities/join?token=${encodeURIComponent(token)}`;
}

/** A pasted link or bare token → the token (null: not a community invite). */
export function tokenFrom(input: string): string | null {
  const raw = input.trim();
  let token = raw;
  try {
    if (/^https?:\/\//i.test(raw)) token = new URL(raw).searchParams.get("token") ?? "";
  } catch {
    return null;
  }
  return /^mcl_[A-Za-z0-9_-]{16,76}$/.test(token) ? token : null;
}

export const LINK_DURATIONS: { label: string; seconds: number }[] = [
  { label: "1 hour", seconds: 3600 },
  { label: "1 day", seconds: 86_400 },
  { label: "7 days", seconds: 604_800 },
  { label: "30 days", seconds: 2_592_000 },
];

// ---------------------------------------------------------------------- chat list

/** Every conversation the user is in that belongs to one of their communities. */
export function communityConversationIds(communities: readonly Pick<Community, "joinedConversationIds">[]): Set<string> {
  const out = new Set<string>();
  for (const c of communities) for (const id of c.joinedConversationIds) out.add(id);
  return out;
}

/** The community a conversation belongs to (by the user's own communities). */
export function communityOf<T extends Pick<Community, "joinedConversationIds">>(communities: readonly T[], conversationId: string): T | null {
  return communities.find((c) => c.joinedConversationIds.includes(conversationId)) ?? null;
}

/** Whether a chat is shown in the All list under the user's preference. */
export const shownInAll = (conversationId: string, communityIds: ReadonlySet<string>, mode: ChatListMode) =>
  mode === "TOGETHER" || !communityIds.has(conversationId);

/** The chats of the Communities filter. */
export const communityChats = <T extends { id: string }>(chats: readonly T[], communityIds: ReadonlySet<string>) =>
  chats.filter((c) => communityIds.has(c.id));

/** Community activity from the chat list snapshot: unread chats, total unread and the latest activity. */
export function communityActivity(
  c: Pick<Community, "joinedConversationIds" | "announcementsConversationId">,
  chats: readonly { id: string; unreadCount: number; lastActivityAt: string }[],
) {
  const mine = chats.filter((x) => c.joinedConversationIds.includes(x.id));
  const latest = mine.reduce<string | null>((at, x) => (!at || x.lastActivityAt > at ? x.lastActivityAt : at), null);
  return { unreadChats: mine.filter((x) => x.unreadCount > 0).length, unread: mine.reduce((n, x) => n + x.unreadCount, 0), lastActivityAt: latest };
}

/** Communities with the most recent activity first (then by name). */
export function sortCommunities<T extends Pick<Community, "name" | "joinedConversationIds" | "announcementsConversationId" | "joinedAt">>(
  communities: readonly T[],
  chats: readonly { id: string; unreadCount: number; lastActivityAt: string }[],
): T[] {
  const at = (c: T) => communityActivity(c, chats).lastActivityAt ?? c.joinedAt;
  return [...communities].sort((a, b) => at(b).localeCompare(at(a)) || a.name.localeCompare(b.name));
}

/** User-facing text of a join outcome. */
export function outcomeText(o: JoinOutcome, what: "community" | "group"): string {
  if (o.outcome === "REQUESTED") return what === "community" ? "Request sent. An admin will review it." : "Request sent to the group's admins.";
  if (o.outcome === "ALREADY_MEMBER") return `You're already in this ${what}.`;
  return what === "community" ? "You joined the community." : "You joined the group.";
}
