/**
 * The typed client of `/v1/messaging/communities` (ADR 0073) over a Messaging relay call.
 * Kept free of app imports so it is tested with a recorded relay; the app binds it to the
 * Meeting Service relay in `services/v2/communities.service.ts`.
 */
import type {
  ChatListMode, Community, CommunityBan, CommunityGroup, CommunityInvitation, CommunityInviteLink, CommunityMember,
  CommunityPreview, CommunityPrivacy, CommunityRole, GroupAccess, JoinOutcome, JoinRequest,
} from "./communities";

export type RelayFn = <T>(method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE", path: string, body?: unknown, idempotencyKey?: string) => Promise<T>;

const BASE = "v1/messaging/communities";
const seg = (id: string) => encodeURIComponent(id);

function query(params: Record<string, string | number | null | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== null && v !== undefined && v !== "") q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
}

export type CreateCommunity = { name: string; description?: string; rules?: string; privacy?: CommunityPrivacy; membersCanAddGroups?: boolean };
export type UpdateCommunity = {
  expectedVersion?: number; name?: string; description?: string | null; rules?: string | null; privacy?: CommunityPrivacy; membersCanAddGroups?: boolean;
};

export function createCommunitiesApi(relay: RelayFn, newKey: () => string = () => `web-${crypto.randomUUID()}`) {
  const c = (id: string) => `${BASE}/${seg(id)}`;
  return {
    list: (cursor?: string | null, limit?: number) =>
      relay<{ communities: Community[]; nextCursor: string | null }>("GET", `${BASE}${query({ limit, cursor })}`),
    create: (body: CreateCommunity, key = newKey()) => relay<Community>("POST", BASE, body, key),
    get: (id: string) => relay<Community>("GET", c(id)),
    update: (id: string, body: UpdateCommunity) => relay<Community>("PATCH", c(id), body),
    remove: (id: string) => relay<{ communityId: string; deleted: boolean }>("DELETE", c(id)),
    transfer: (id: string, principalId: string) => relay<Community>("POST", `${c(id)}/owner`, { principalId }),
    leave: (id: string) => relay<{ communityId: string; left: boolean }>("POST", `${c(id)}/leave`, {}),

    members: (id: string, cursor?: string | null, limit?: number) =>
      relay<{ members: CommunityMember[]; nextCursor: string | null }>("GET", `${c(id)}/members${query({ limit, cursor })}`),
    setRole: (id: string, principalId: string, role: Exclude<CommunityRole, "OWNER">) =>
      relay<CommunityMember>("PUT", `${c(id)}/members/${seg(principalId)}/role`, { role }),
    removeMember: (id: string, principalId: string) => relay<{ principalId: string; state: string }>("DELETE", `${c(id)}/members/${seg(principalId)}`),

    bans: (id: string) => relay<{ bans: CommunityBan[] }>("GET", `${c(id)}/bans`),
    ban: (id: string, principalId: string) => relay<CommunityBan>("POST", `${c(id)}/bans`, { principalId }),
    unban: (id: string, principalId: string) => relay<{ principalId: string; lifted: boolean }>("DELETE", `${c(id)}/bans/${seg(principalId)}`),

    invitations: (id: string) => relay<{ invitations: CommunityInvitation[] }>("GET", `${c(id)}/invitations`),
    invite: (id: string, principalIds: string[]) =>
      relay<{ invitations: CommunityInvitation[]; skipped: { principalId: string; reason: string }[] }>("POST", `${c(id)}/invitations`, { principalIds }),
    revokeInvitation: (id: string, invitationId: string) => relay<CommunityInvitation>("DELETE", `${c(id)}/invitations/${seg(invitationId)}`),
    myInvitations: () => relay<{ invitations: CommunityInvitation[] }>("GET", `${BASE}/invitations`),
    acceptInvitation: (invitationId: string) => relay<JoinOutcome>("POST", `${BASE}/invitations/${seg(invitationId)}/accept`, {}),
    declineInvitation: (invitationId: string) => relay<CommunityInvitation>("POST", `${BASE}/invitations/${seg(invitationId)}/decline`, {}),

    inviteLinks: (id: string) => relay<{ inviteLinks: CommunityInviteLink[] }>("GET", `${c(id)}/invite-links`),
    createInviteLink: (id: string, opts: { expiresInSeconds?: number; maxUses?: number } = {}) =>
      relay<CommunityInviteLink>("POST", `${c(id)}/invite-links`, opts),
    revokeInviteLink: (id: string, linkId: string) => relay<CommunityInviteLink>("POST", `${c(id)}/invite-links/${seg(linkId)}/revoke`, {}),
    previewLink: (token: string) => relay<CommunityPreview>("POST", `${BASE}/invite-links/preview`, { token }),
    joinByLink: (token: string, message?: string) =>
      relay<JoinOutcome>("POST", `${BASE}/invite-links/join`, message?.trim() ? { token, message: message.trim() } : { token }),

    joinRequests: (id: string, state: JoinRequest["state"] | null = "PENDING", cursor?: string | null) =>
      relay<{ requests: JoinRequest[]; nextCursor: string | null }>("GET", `${c(id)}/join-requests${query({ state, cursor })}`),
    approve: (id: string, requestId: string) => relay<JoinRequest>("POST", `${c(id)}/join-requests/${seg(requestId)}/approve`, {}),
    reject: (id: string, requestId: string) => relay<JoinRequest>("POST", `${c(id)}/join-requests/${seg(requestId)}/reject`, {}),
    myRequests: () => relay<{ requests: JoinRequest[]; nextCursor: string | null }>("GET", `${BASE}/join-requests`),
    cancelRequest: (requestId: string) => relay<JoinRequest>("POST", `${BASE}/join-requests/${seg(requestId)}/cancel`, {}),

    groups: (id: string) => relay<{ groups: CommunityGroup[] }>("GET", `${c(id)}/groups`),
    createGroup: (id: string, body: { title: string; description?: string; access?: GroupAccess }, key = newKey()) =>
      relay<CommunityGroup>("POST", `${c(id)}/groups`, body, key),
    linkGroup: (id: string, conversationId: string, access?: GroupAccess) =>
      relay<CommunityGroup>("POST", `${c(id)}/groups/link`, access ? { conversationId, access } : { conversationId }),
    orderGroups: (id: string, conversationIds: string[]) => relay<{ groups: CommunityGroup[] }>("PUT", `${c(id)}/groups/order`, { conversationIds }),
    updateGroup: (id: string, conversationId: string, access: GroupAccess) =>
      relay<CommunityGroup>("PATCH", `${c(id)}/groups/${seg(conversationId)}`, { access }),
    unlinkGroup: (id: string, conversationId: string) =>
      relay<{ conversationId: string; unlinked: boolean }>("DELETE", `${c(id)}/groups/${seg(conversationId)}`),
    joinGroup: (id: string, conversationId: string, message?: string) =>
      relay<JoinOutcome>("POST", `${c(id)}/groups/${seg(conversationId)}/join`, message?.trim() ? { message: message.trim() } : {}),

    preferences: () => relay<{ chatListMode: ChatListMode }>("GET", `${BASE}/preferences`),
    setPreferences: (chatListMode: ChatListMode) => relay<{ chatListMode: ChatListMode }>("PUT", `${BASE}/preferences`, { chatListMode }),
  };
}

export type CommunitiesApi = ReturnType<typeof createCommunitiesApi>;
