/**
 * Every HTTP operation the mobile app calls, in one table (MTG-001 / MTG-003).
 *
 * Paths are relative to the API base of their version (`API_URL` = `…/api/v1`,
 * `API_V2_URL` = `…/api/v2`). API modules must build URLs only through this table: the unit
 * test `tests/routes.test.ts` checks that every entry is an operation (method + path) of the
 * served contract `packages/contracts/openapi/meeting-service.json`, and that no API module
 * writes a raw path. No backend alias exists for any old mobile path.
 *
 * Pure and dependency-free (runs under `node --test`).
 */

export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
export type ApiVersion = "v1" | "v2";

export interface Route<A extends string[] = string[]> {
  method: HttpMethod;
  version: ApiVersion;
  path: (...args: A) => string;
}

/** Encodes one path segment. */
export const seg = (value: string): string => encodeURIComponent(value);

function r<A extends string[]>(method: HttpMethod, version: ApiVersion, path: (...args: A) => string): Route<A> {
  return { method, version, path };
}

const conf = (id: string) => `/conferences/${seg(id)}`;
const space = (id: string) => `/meeting-spaces/${seg(id)}`;

export const ROUTES = {
  // ---- Account (v1 auth; the account service is still served under /api/v1) -------------
  login: r("POST", "v1", () => "/auth/login"),
  register: r("POST", "v1", () => "/auth/register"),
  verifyEmail: r("POST", "v1", () => "/auth/verify-email"),
  resendVerification: r("POST", "v1", () => "/auth/resend-verification"),
  forgotPassword: r("POST", "v1", () => "/auth/forgot-password"),
  verifyResetToken: r("POST", "v1", () => "/auth/verify-reset-token"),
  resetPassword: r("POST", "v1", () => "/auth/reset-password"),
  refresh: r("POST", "v1", () => "/auth/refresh"),
  logout: r("POST", "v1", () => "/auth/logout"),
  changePassword: r("POST", "v1", () => "/auth/change-password"),
  googleExchange: r("POST", "v1", () => "/auth/oauth/google/exchange"),

  // ---- Me (v2) --------------------------------------------------------------------------
  myProfile: r("GET", "v2", () => "/me/profile"),
  updateMyProfile: r("PATCH", "v2", () => "/me/profile"),
  mySessions: r("GET", "v2", () => "/me/sessions"),
  revokeMySession: r("POST", "v2", (sessionId: string) => `/me/sessions/${seg(sessionId)}/revoke`),
  revokeAllMySessions: r("POST", "v2", () => "/me/sessions/revoke-all"),
  myWorkspaces: r("GET", "v2", () => "/me/workspaces"),
  myWorkspaceCapabilities: r(
    "GET",
    "v2",
    (workspaceId: string) => `/me/workspaces/${seg(workspaceId)}/capabilities`
  ),
  myOrganizations: r("GET", "v2", () => "/me/organizations"),
  myConferences: r("GET", "v2", () => "/me/conferences"),

  // ---- Organizations (v2; administration stays web-first) ---------------------------------
  acceptOrganizationInvitation: r("POST", "v2", () => "/organization-invitations/accept"),
  declineOrganizationInvitation: r("POST", "v2", () => "/organization-invitations/decline"),
  leaveOrganization: r("POST", "v2", (orgId: string) => `/organizations/${seg(orgId)}/members/me/leave`),

  // ---- Meeting spaces ----------------------------------------------------------------------
  listSpaces: r("GET", "v2", () => "/meeting-spaces"),
  createSpace: r("POST", "v2", () => "/meeting-spaces"),
  getSpace: r("GET", "v2", (id: string) => space(id)),
  updateSpace: r("PATCH", "v2", (id: string) => space(id)),
  archiveSpace: r("POST", "v2", (id: string) => `${space(id)}/archive`),
  listConferences: r("GET", "v2", (id: string) => `${space(id)}/conferences`),
  createConference: r("POST", "v2", (id: string) => `${space(id)}/conferences`),
  instantMeeting: r("POST", "v2", () => "/instant-meetings"),
  resolve: r("GET", "v2", (identifier: string) => `/resolve/${seg(identifier)}`),

  // ---- Conferences -------------------------------------------------------------------------
  getConference: r("GET", "v2", (id: string) => conf(id)),
  cancelConference: r("POST", "v2", (id: string) => `${conf(id)}/cancel`),
  rescheduleConference: r("POST", "v2", (id: string) => `${conf(id)}/reschedule`),
  startConference: r("POST", "v2", (id: string) => `${conf(id)}/start`),
  endConference: r("POST", "v2", (id: string) => `${conf(id)}/end`),
  lockConference: r("POST", "v2", (id: string) => `${conf(id)}/lock`),
  unlockConference: r("POST", "v2", (id: string) => `${conf(id)}/unlock`),
  preJoin: r("GET", "v2", (id: string) => `${conf(id)}/pre-join`),
  join: r("POST", "v2", (id: string) => `${conf(id)}/join`),
  leave: r("POST", "v2", (id: string) => `${conf(id)}/leave`),
  mediaCredential: r("POST", "v2", (id: string) => `${conf(id)}/media-credential`),

  // ---- Participants and admission ----------------------------------------------------------
  participants: r("GET", "v2", (id: string) => `${conf(id)}/participants`),
  me: r("GET", "v2", (id: string) => `${conf(id)}/participants/me`),
  waitingRoom: r("GET", "v2", (id: string) => `${conf(id)}/waiting-room`),
  admit: r("POST", "v2", (id: string) => `${conf(id)}/waiting-room/admit`),
  deny: r("POST", "v2", (id: string) => `${conf(id)}/waiting-room/deny`),
  removeParticipant: r(
    "POST",
    "v2",
    (id: string, participantId: string) => `${conf(id)}/participants/${seg(participantId)}/remove`
  ),
  muteParticipant: r(
    "POST",
    "v2",
    (id: string, participantId: string) => `${conf(id)}/participants/${seg(participantId)}/mute`
  ),
  changeRole: r(
    "PATCH",
    "v2",
    (id: string, participantId: string) => `${conf(id)}/participants/${seg(participantId)}/role`
  ),

  // ---- In-meeting chat (MSG-012: the v2 conference chat, not legacy /meetings/{id}/chat) -----
  messages: r("GET", "v2", (id: string) => `${conf(id)}/messages`),
  sendMessage: r("POST", "v2", (id: string) => `${conf(id)}/messages`),
  deleteMessage: r(
    "DELETE",
    "v2",
    (id: string, messageId: string) => `${conf(id)}/messages/${seg(messageId)}`
  ),

  // ---- Engagement ----------------------------------------------------------------------------
  sendReaction: r("POST", "v2", (id: string) => `${conf(id)}/reactions`),
  raisedHands: r("GET", "v2", (id: string) => `${conf(id)}/hand-raise`),
  raiseHand: r("POST", "v2", (id: string) => `${conf(id)}/hand-raise`),
  lowerHand: r("DELETE", "v2", (id: string) => `${conf(id)}/hand-raise`),
  lowerOtherHand: r(
    "DELETE",
    "v2",
    (id: string, participantId: string) => `${conf(id)}/participants/${seg(participantId)}/hand-raise`
  ),
  polls: r("GET", "v2", (id: string) => `${conf(id)}/polls`),
  createPoll: r("POST", "v2", (id: string) => `${conf(id)}/polls`),
  getPoll: r("GET", "v2", (id: string, pollId: string) => `${conf(id)}/polls/${seg(pollId)}`),
  openPoll: r("POST", "v2", (id: string, pollId: string) => `${conf(id)}/polls/${seg(pollId)}/open`),
  closePoll: r("POST", "v2", (id: string, pollId: string) => `${conf(id)}/polls/${seg(pollId)}/close`),
  archivePoll: r("POST", "v2", (id: string, pollId: string) => `${conf(id)}/polls/${seg(pollId)}/archive`),
  submitPollResponse: r(
    "POST",
    "v2",
    (id: string, pollId: string) => `${conf(id)}/polls/${seg(pollId)}/responses`
  ),
  pollResults: r("GET", "v2", (id: string, pollId: string) => `${conf(id)}/polls/${seg(pollId)}/results`),
  questions: r("GET", "v2", (id: string) => `${conf(id)}/questions`),
  submitQuestion: r("POST", "v2", (id: string) => `${conf(id)}/questions`),
  voteQuestion: r("POST", "v2", (id: string, qid: string) => `${conf(id)}/questions/${seg(qid)}/vote`),
  unvoteQuestion: r("DELETE", "v2", (id: string, qid: string) => `${conf(id)}/questions/${seg(qid)}/vote`),
  answerQuestion: r("POST", "v2", (id: string, qid: string) => `${conf(id)}/questions/${seg(qid)}/answer`),
  dismissQuestion: r("POST", "v2", (id: string, qid: string) => `${conf(id)}/questions/${seg(qid)}/dismiss`),
  pinQuestion: r("POST", "v2", (id: string, qid: string) => `${conf(id)}/questions/${seg(qid)}/pin`),
  unpinQuestion: r("DELETE", "v2", (id: string, qid: string) => `${conf(id)}/questions/${seg(qid)}/pin`),
};

export type RouteName = keyof typeof ROUTES;

/** The contract path template of a route (`{}` for every parameter), for contract checks. */
export function routeTemplate(route: { version: ApiVersion; path: (...args: string[]) => string }): string {
  const marker = "\u0000";
  const args = new Array(route.path.length).fill(marker);
  return `/api/${route.version}${route.path(...args)}`.split(encodeURIComponent(marker)).join("{}");
}
