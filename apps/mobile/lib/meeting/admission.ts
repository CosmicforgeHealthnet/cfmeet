/**
 * Join / admission state machine for the mobile room (MTG-002, FE-005).
 *
 * Flow (all Meeting Service v2, canonical paths only):
 * 1. `POST /conferences/{id}/join` once. A guest's response carries `participantToken` once:
 *    the app keeps it (SecureStore, bound to that conference) and sends it as
 *    `X-Participant-Token` on every later call for that conference.
 * 2. WAITING (`rtcStatus = NOT_ISSUED_WAITING`): poll the read-only
 *    `GET /conferences/{id}/participants/me` (never re-POST join on a timer).
 * 3. Once ADMITTED: one more `POST /join` with `previousSessionId` returns the RTC credential
 *    for the same participant (the credential identifies it, so no duplicate is created).
 * 4. DENIED / REMOVED / BANNED, or a terminal conference: stop and show the server's message.
 *
 * Pure; runs under `node --test`.
 */
import type {
  AdmissionState,
  ConferenceStatus,
  JoinConferenceResponse,
  JoinPermissions,
  ParticipantResponse,
  Role,
} from "@/lib/api/generated/meeting-service";

export interface LiveMedia {
  conferenceId: string;
  meetingSpaceId: string;
  participantId: string;
  participantSessionId: string | null;
  role: Role;
  displayName: string;
  permissions: JoinPermissions;
  serverUrl: string;
  token: string;
  provider: string;
  expiresAt: string;
  recordingActive: boolean;
  transcriptionActive: boolean;
}

export type JoinOutcome =
  | { kind: "live"; media: LiveMedia }
  | { kind: "waiting"; participantId: string; participantSessionId: string | null }
  | { kind: "refused"; reason: string }
  | { kind: "media_unavailable"; reason: string };

const TERMINAL_CONFERENCE: ConferenceStatus[] = ["ENDED", "CANCELLED", "FAILED"];

export function isTerminalConference(status: ConferenceStatus | null | undefined): boolean {
  return !!status && TERMINAL_CONFERENCE.includes(status);
}

export function refusalMessage(state: AdmissionState, denialMessage?: string | null): string {
  switch (state) {
    case "DENIED":
      return denialMessage?.trim() || "The host did not admit you to this meeting.";
    case "REMOVED":
      return "You were removed from this meeting.";
    case "BANNED":
      return "You can no longer join this meeting.";
    default:
      return "You cannot join this meeting.";
  }
}

/** Classifies a `POST /join` response. */
export function classifyJoin(response: JoinConferenceResponse): JoinOutcome {
  const p = response.participant;
  if (p.admissionState === "DENIED" || p.admissionState === "REMOVED" || p.admissionState === "BANNED") {
    return { kind: "refused", reason: refusalMessage(p.admissionState, p.denialMessage) };
  }
  if (isTerminalConference(response.conference.status)) {
    return { kind: "refused", reason: "This meeting has ended." };
  }
  if (response.rtc && response.rtcStatus === "ISSUED" && p.admissionState === "ADMITTED") {
    return {
      kind: "live",
      media: {
        conferenceId: response.conference.id,
        meetingSpaceId: response.conference.meetingSpaceId,
        participantId: p.id,
        participantSessionId: response.participantSession?.id ?? null,
        role: p.role,
        displayName: p.displayName,
        permissions: response.permissions,
        serverUrl: response.rtc.url,
        token: response.rtc.credential,
        provider: response.rtc.provider,
        expiresAt: response.rtc.expiresAt,
        recordingActive: response.policy.recordingActive,
        transcriptionActive: response.policy.transcriptionActive,
      },
    };
  }
  if (response.rtcStatus === "NOT_ISSUED_WAITING" && p.admissionState === "WAITING") {
    return {
      kind: "waiting",
      participantId: p.id,
      participantSessionId: response.participantSession?.id ?? null,
    };
  }
  if (response.rtcStatus === "PROVIDER_CONTROL_DISABLED") {
    return {
      kind: "media_unavailable",
      reason: "Audio and video are not available for this meeting right now.",
    };
  }
  return { kind: "media_unavailable", reason: "Media access is unavailable for this meeting." };
}

export type WaitingStep =
  | { kind: "keep_waiting" }
  | { kind: "admitted" }
  | { kind: "refused"; reason: string };

/** Next step after a `GET /participants/me` poll while waiting. */
export function nextWaitingStep(me: Pick<ParticipantResponse, "admissionState" | "denialMessage">): WaitingStep {
  switch (me.admissionState) {
    case "WAITING":
      return { kind: "keep_waiting" };
    case "ADMITTED":
      return { kind: "admitted" };
    default:
      return { kind: "refused", reason: refusalMessage(me.admissionState, me.denialMessage) };
  }
}

/** Poll delay with gentle backoff on consecutive failures (3 s → max 30 s). */
export function waitingPollDelayMs(consecutiveFailures: number): number {
  const base = 3000;
  return Math.min(30000, base * Math.pow(2, Math.max(0, Math.min(consecutiveFailures, 4))));
}

/** Whether a poll failure (HTTP status, 0 = network) should stop waiting. */
export function isFatalPollFailure(status: number): boolean {
  return status === 401 || status === 403 || status === 404 || status === 410;
}

export interface AdmissionPollDeps {
  /** `GET /conferences/{id}/participants/me` (read-only). */
  fetchMe: () => Promise<Pick<ParticipantResponse, "admissionState" | "denialMessage">>;
  /** Waits `ms` (or resolves early when aborted). */
  sleep: (ms: number) => Promise<void>;
  /** HTTP status of a failure (0 for network errors). */
  statusOf: (error: unknown) => number;
  isAborted: () => boolean;
  /** Called after every poll (UI progress / diagnostics; never receives credentials). */
  onPoll?: (info: { attempt: number; consecutiveFailures: number }) => void;
}

export type AdmissionPollResult =
  | { kind: "admitted" }
  | { kind: "refused"; reason: string }
  | { kind: "failed"; status: number }
  | { kind: "aborted" };

/**
 * Waits in the waiting room by polling the read-only participant state (FE-005). It never
 * re-POSTs join; the caller does exactly one `POST /join` after `admitted` to obtain media.
 */
export async function pollAdmission(deps: AdmissionPollDeps): Promise<AdmissionPollResult> {
  let failures = 0;
  let attempt = 0;
  for (;;) {
    await deps.sleep(waitingPollDelayMs(failures));
    if (deps.isAborted()) return { kind: "aborted" };
    attempt += 1;
    try {
      const me = await deps.fetchMe();
      failures = 0;
      deps.onPoll?.({ attempt, consecutiveFailures: failures });
      if (deps.isAborted()) return { kind: "aborted" };
      const step = nextWaitingStep(me);
      if (step.kind === "admitted") return { kind: "admitted" };
      if (step.kind === "refused") return { kind: "refused", reason: step.reason };
    } catch (error) {
      if (deps.isAborted()) return { kind: "aborted" };
      const status = deps.statusOf(error);
      if (isFatalPollFailure(status)) return { kind: "failed", status };
      failures += 1;
      deps.onPoll?.({ attempt, consecutiveFailures: failures });
    }
  }
}
