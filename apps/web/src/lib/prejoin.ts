/**
 * Pre-join and admission rules of the web room (FE-001, FE-014, FE-017).
 *
 * `GET /api/v2/conferences/{id}/pre-join` (OpenAPI `PreJoinResponse`) drives the inputs:
 * `access.passcodeRequired / invitationRequired / authenticationRequired / waitingRoomExpected`
 * and `consents.required` (minus `consents.granted`) at `consents.version`. The join request
 * sends `consents: [{ type, version }]`; the backend refuses a join without every required
 * consent (`403 CONSENT_REQUIRED`).
 *
 * The live room is keyed by the conference id (`/conferences/{conferenceId}`); the room only
 * talks to v2 conference endpoints and LiveKit — there is no legacy v1 meeting branch.
 *
 * Pure: type-only imports, so node tests run it directly (tests/prejoin-room.test.mjs).
 */
import type {
  ConferenceStatus,
  ConsentGrant,
  ConsentKind,
  JoinConferenceResponse,
  PreJoinResponse,
  Role,
} from "../types/generated/meeting-service";
// The room components consume the hand-written v2 view types (narrower enums).
import type { JoinPermissions, MediaContextView } from "../types/v2";

/** What the room needs after admission; built only from a v2 join response. */
export interface RoomJoinData {
  conferenceId: string;
  meetingSpaceId: string;
  participantId: string;
  participantSessionId?: string;
  conferenceRole: Role;
  permissions?: JoinPermissions;
  mediaContext?: MediaContextView;
  isGuest: boolean;
  /** LiveKit access token and URL issued by the backend (never minted client-side). */
  rtcToken: string;
  rtcUrl: string;
}

export const MANAGING_ROLES: Role[] = ["OWNER", "HOST", "CO_HOST"];

/**
 * Maps a join response: room data when media credentials were issued, `null` when the caller
 * waits for admission (`rtcStatus: NOT_ISSUED_WAITING`), an error otherwise.
 */
export function roomJoinData(response: Pick<JoinConferenceResponse, "conference" | "participant" | "participantSession" | "permissions" | "mediaContext" | "rtc" | "rtcStatus">, isGuest: boolean): RoomJoinData | null {
  if (!response.rtc) {
    if (response.rtcStatus === "NOT_ISSUED_WAITING" && response.participant.admissionState === "WAITING") return null;
    throw new Error("Media access is unavailable for this conference.");
  }
  return {
    conferenceId: response.conference.id,
    meetingSpaceId: response.conference.meetingSpaceId,
    participantId: response.participant.id,
    participantSessionId: response.participantSession?.id ?? undefined,
    conferenceRole: response.participant.role,
    permissions: response.permissions as unknown as JoinPermissions,
    mediaContext: response.mediaContext as unknown as MediaContextView,
    isGuest,
    rtcToken: response.rtc.credential,
    rtcUrl: response.rtc.url,
  };
}

export interface PreJoinRequirements {
  passcode: boolean;
  invitation: boolean;
  signIn: boolean;
  waitingRoom: boolean;
  externalAuthorization: boolean;
  alreadyAuthorized: boolean;
  /** Consents still to grant (required and not already granted), in backend order. */
  consents: ConsentKind[];
  consentVersion: string;
}

/**
 * Inputs the pre-join must show. While the pre-join read is unavailable the passcode falls
 * back to the resolved space's `requiresPasscode`; nothing else is guessed.
 */
export function preJoinRequirements(preJoin: PreJoinResponse | null | undefined, { signedIn, spaceRequiresPasscode = false, hasInvitationToken = false }: { signedIn: boolean; spaceRequiresPasscode?: boolean; hasInvitationToken?: boolean }): PreJoinRequirements {
  const access = preJoin?.access;
  const granted = new Set(preJoin?.consents?.granted ?? []);
  const authorized = !!access?.alreadyAuthorized;
  return {
    passcode: !authorized && (access ? access.passcodeRequired : spaceRequiresPasscode),
    invitation: !authorized && !!access?.invitationRequired && !hasInvitationToken,
    signIn: !!access?.authenticationRequired && !signedIn,
    waitingRoom: !!access?.waitingRoomExpected,
    externalAuthorization: !!access?.externalAuthorization,
    alreadyAuthorized: authorized,
    consents: (preJoin?.consents?.required ?? []).filter((kind) => !granted.has(kind)),
    consentVersion: preJoin?.consents?.version ?? "",
  };
}

/** The join request's `consents`: every checked required consent at the pre-join version. */
export function consentGrants(requirements: Pick<PreJoinRequirements, "consents" | "consentVersion">, checked: Partial<Record<ConsentKind, boolean>>): ConsentGrant[] {
  return requirements.consents.filter((kind) => checked[kind]).map((type) => ({ type, version: requirements.consentVersion }));
}

/** Why Join is disabled (null = enabled). */
export function joinBlocker(requirements: PreJoinRequirements, input: { displayName: string; passcode: string; invitationToken: string; checked: Partial<Record<ConsentKind, boolean>> }): string | null {
  if (requirements.signIn) return "Sign in to join this meeting.";
  if (!input.displayName.trim()) return "Enter your name.";
  if (requirements.passcode && !input.passcode.trim()) return "Enter the meeting passcode.";
  if (requirements.invitation && !input.invitationToken.trim()) return "Enter your invitation.";
  if (requirements.consents.some((kind) => !input.checked[kind])) return "Review and accept the required notices.";
  return null;
}

export const CONSENT_TEXT: Record<ConsentKind, string> = {
  RECORDING: "I agree that this meeting may be recorded.",
  TRANSCRIPTION: "I agree that this meeting may be transcribed.",
  AI_PROCESSING: "I agree to AI processing of this meeting (for example summaries).",
  DATA_SHARING: "I agree to the data sharing described by the organizer.",
  HEALTHCARE_TERMS: "I accept the healthcare terms of this consultation.",
  PRIVACY_NOTICE: "I have read the privacy notice.",
};

const CLOSED: ConferenceStatus[] = ["ENDING", "ENDED", "CANCELLED", "FAILED"];

export type RoomPhase = "closed" | "live" | "not-started";

export function roomPhase(status: ConferenceStatus | undefined | null): RoomPhase {
  if (!status) return "not-started";
  if (CLOSED.includes(status)) return "closed";
  if (status === "LIVE") return "live";
  return "not-started";
}

/** Error code of a backend error envelope. */
export function errorCode(error: unknown): string | undefined {
  return (error as { response?: { data?: { error?: { code?: string } } } })?.response?.data?.error?.code;
}

/** User-facing text for admission errors; the backend message is kept for anything else. */
export function joinErrorMessage(error: unknown): string {
  const code = errorCode(error);
  switch (code) {
    case "CONSENT_REQUIRED": return "This meeting requires your consent before you can join. Review the notices above.";
    case "MEETING_PASSCODE_REQUIRED": return "Enter the meeting passcode.";
    case "MEETING_PASSCODE_INVALID": return "The passcode is not correct.";
    case "INVITATION_REQUIRED": return "This meeting is by invitation. Enter your invitation or sign in with the invited account.";
    case "INVITATION_INVALID":
    case "INVITATION_EXPIRED": return "This invitation is not valid any more. Ask the organizer for a new one.";
    case "AUTHENTICATION_REQUIRED": return "Sign in to join this meeting.";
    case "CONFERENCE_NOT_LIVE": return "The meeting has not started yet. You can join once the host starts it.";
    case "CONFERENCE_LOCKED": return "The meeting is locked. Ask the host to let you in.";
    case "PARTICIPANT_BANNED": return "You cannot join this meeting.";
    case "MEETING_CAPACITY_REACHED": return "The meeting is full.";
    case "MEETING_ACCESS_DENIED":
    case "PERMISSION_DENIED":
    case "PROFILE_POLICY_DENIED": return "You are not allowed to join this meeting.";
    case "REVOCATION_CHECK_UNAVAILABLE": return "Your session could not be verified right now. Please retry in a moment.";
    default: {
      const message = (error as { response?: { data?: { error?: { message?: string } } } })?.response?.data?.error?.message;
      return message || (error instanceof Error ? error.message : "Could not join the meeting.");
    }
  }
}

/** The pre-join / live room route of a conference (FE-017); the meeting code stays the share link. */
export function conferenceRoomPath(conferenceId: string, invitationToken?: string): string {
  const base = `/conferences/${encodeURIComponent(conferenceId)}`;
  // Invitation tokens travel in the fragment so they never reach server logs.
  return invitationToken ? `${base}#${encodeURIComponent(invitationToken)}` : base;
}
