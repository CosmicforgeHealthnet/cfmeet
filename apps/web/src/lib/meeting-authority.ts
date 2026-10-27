import type { AdmissionState, Role, RolePolicy, CreateInvitationRequest, InvitationResponse } from "@/types/v2";

const ranks: Record<Role, number> = { OWNER: 7, HOST: 6, CO_HOST: 5, MODERATOR: 4, PRESENTER: 3, PARTICIPANT: 2, VIEWER: 1 };
export function effectiveConferenceRole(spaceRole?: Role, participantRole?: Role, admissionState?: AdmissionState): Role | undefined {
  if (admissionState && ["REMOVED", "BANNED", "DENIED"].includes(admissionState)) return undefined;
  if (!spaceRole) return participantRole;
  if (!participantRole) return spaceRole;
  return ranks[spaceRole] >= ranks[participantRole] ? spaceRole : participantRole;
}

export function invitationAvailability(inv: Pick<InvitationResponse, "status" | "expiresAt" | "usageCount" | "useLimit">, now: number) {
  if (inv.status === "REVOKED") return "Revoked";
  if (Date.parse(inv.expiresAt) <= now) return "Expired";
  if (inv.useLimit != null && inv.usageCount >= inv.useLimit) return "Use limit reached";
  return "Active";
}
export type MeetingPermission =
  | "participant.admit"
  | "participant.deny"
  | "participant.return_to_waiting"
  | "participant.remove"
  | "participant.ban"
  | "participant.mute"
  | "participant.audio_lock"
  | "participant.video_lock"
  | "participant.screen_share_stop"
  | "participant.spotlight"
  | "participant.role_change"
  | "hand.lower_other"
  | "chat.moderate"
  | "conference.lock"
  | "conference.end"
  | "conference.configure_live"
  | "cohost.assign";
export function meetingAllows(role: Role | undefined, permission: MeetingPermission, policy?: RolePolicy) {
  if (!role) return false;
  if (role === "OWNER" || role === "HOST") return true;
  if (role === "CO_HOST") {
    if ([
      "participant.admit",
      "participant.deny",
      "participant.return_to_waiting",
      "participant.remove",
      "participant.mute",
      "participant.audio_lock",
      "participant.video_lock",
      "participant.screen_share_stop",
      "participant.spotlight",
      "participant.role_change",
      "hand.lower_other",
      "chat.moderate",
      "conference.lock",
      "conference.configure_live",
    ].includes(permission)) return true;
    if (permission === "conference.end") return policy?.overrides?.[role]?.[permission] ?? false;
    return policy?.overrides?.[role]?.[permission] ?? false;
  }
  if (role === "MODERATOR") {
    if (permission === "participant.remove") return true;
    if (permission === "cohost.assign") return false;
    if (permission === "participant.deny" || permission === "participant.return_to_waiting") {
      return policy?.overrides?.[role]?.[permission] ?? true;
    }
    if (["participant.audio_lock", "participant.video_lock", "participant.screen_share_stop", "participant.spotlight", "hand.lower_other", "conference.configure_live"].includes(permission)) {
      return policy?.overrides?.[role]?.[permission] ?? true;
    }
    if (permission === "conference.lock" || permission === "conference.end") {
      return policy?.overrides?.[role]?.[permission] ?? false;
    }
    if (permission === "chat.moderate") return true;
    return policy?.overrides?.[role]?.[permission] ?? ["participant.admit", "participant.mute"].includes(permission);
  }
  return false;
}
export function mayTarget(actor: Role | undefined, target: Role, self: boolean) {
  return !!actor && !self && target !== "OWNER" && ranks[target] < ranks[actor];
}
export function invitationRoles(actor?: Role, policy?: RolePolicy): Role[] {
  if (!actor || !meetingAllows(actor, "participant.admit", policy)) return [];
  return (Object.keys(ranks) as Role[]).filter(role => role !== "OWNER" &&
    (role !== "CO_HOST" || meetingAllows(actor, "cohost.assign", policy)) &&
    (ranks[role] < ranks[actor] || (role === "CO_HOST" && ranks[actor] >= ranks.CO_HOST)));
}
export function participantRoles(actor?: Role, policy?: RolePolicy): Role[] {
  if (!meetingAllows(actor, "participant.role_change", policy) || !actor) return [];
  return (Object.keys(ranks) as Role[]).filter(role => role !== "OWNER" &&
    (role !== "CO_HOST" || meetingAllows(actor, "cohost.assign", policy)) &&
    (ranks[role] < ranks[actor] || (role === "CO_HOST" && ranks[actor] >= ranks.CO_HOST)));
}
export function invitationToken(input: string) {
  let token = input.trim();
  if (/^https?:\/\//i.test(token)) token = new URL(token).hash.slice(1);
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(token)) throw new Error("Enter a valid invitation token or invitation link.");
  return token;
}
export function invitationInput(values: { role: Role; conferenceId: string; email: string; hours: string; limit: string }): CreateInvitationRequest {
  const hours = Number(values.hours), limit = Number(values.limit);
  if (!Number.isSafeInteger(hours) || hours < 1) throw new Error("Expiry must be a positive number of hours. The server applies the configured maximum.");
  if (values.limit && (!Number.isInteger(limit) || limit < 1 || limit > 10000)) throw new Error("Use limit must be between 1 and 10000, or left empty.");
  const email = values.email.trim();
  if (email && (!/^[^\s@]+@[^\s@]+$/.test(email) || email.length > 320)) throw new Error("Enter a valid email address.");
  if (values.role === "OWNER") throw new Error("Ownership requires a separate transfer.");
  return { role: values.role, expiresInHours: hours, ...(values.conferenceId ? { conferenceId: values.conferenceId } : {}), ...(email ? { inviteeEmail: email.toLowerCase() } : {}), ...(values.limit ? { useLimit: limit } : {}) };
}
