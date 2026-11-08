/**
 * Which in-meeting controls to show (UI only — the server re-checks every action).
 *
 * Mirrors the Meeting Service default role matrix used by the web client
 * (`apps/web/src/lib/meeting-authority.ts`), limited to the controls the mobile room offers.
 * A role-policy override can only be known server-side, so a hidden control never grants
 * anything and a shown control can still be refused (the 403 is decoded and shown).
 *
 * Pure; runs under `node --test`.
 */
import type { AdmissionState, Role } from "@/lib/api/generated/meeting-service";

export const ROLE_RANK: Record<Role, number> = {
  OWNER: 7,
  HOST: 6,
  CO_HOST: 5,
  MODERATOR: 4,
  PRESENTER: 3,
  PARTICIPANT: 2,
  VIEWER: 1,
};

export type MobileMeetingPermission =
  | "participant.admit"
  | "participant.deny"
  | "participant.remove"
  | "participant.mute"
  | "conference.lock"
  | "conference.end"
  | "chat.moderate";

/** The stronger of the space role and the participant role; none once removed/denied/banned. */
export function effectiveRole(
  spaceRole: Role | null | undefined,
  participantRole: Role | null | undefined,
  admissionState?: AdmissionState | null
): Role | undefined {
  if (admissionState && (admissionState === "REMOVED" || admissionState === "BANNED" || admissionState === "DENIED")) {
    return undefined;
  }
  if (!spaceRole) return participantRole ?? undefined;
  if (!participantRole) return spaceRole;
  return ROLE_RANK[spaceRole] >= ROLE_RANK[participantRole] ? spaceRole : participantRole;
}

export function meetingAllows(role: Role | undefined, permission: MobileMeetingPermission): boolean {
  if (!role) return false;
  if (role === "OWNER" || role === "HOST") return true;
  if (role === "CO_HOST") return permission !== "conference.end";
  if (role === "MODERATOR") {
    return (
      permission === "participant.admit" ||
      permission === "participant.deny" ||
      permission === "participant.remove" ||
      permission === "participant.mute" ||
      permission === "chat.moderate"
    );
  }
  return false;
}

/** Whether `actor` may act on a participant with role `target` (never self, never the owner). */
export function mayTarget(actor: Role | undefined, target: Role, self: boolean): boolean {
  return !!actor && !self && target !== "OWNER" && ROLE_RANK[target] < ROLE_RANK[actor];
}

export function roleLabel(role: Role | string | null | undefined): string {
  if (!role) return "";
  return role
    .toLowerCase()
    .split("_")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join("-");
}
