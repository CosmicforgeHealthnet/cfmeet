/**
 * Who may control a Call (CAL-002) — ADR 0009 amendment "Who may control a Call".
 *
 * The Calling API authorizes commands at application scope only; the CF Meet adapter acts
 * for its users and therefore applies these party rules (the same module also decides which
 * buttons the web UI renders, so the UI and the adapter can never disagree):
 *
 *  - leave (remove one's own session): any participant with a session of their own;
 *  - end for all, hold, resume: any ACTIVE participant (not ended) — caller, callee or a
 *    transfer target, so a transferred call stays controllable;
 *  - transfer: an active participant transferring its OWN place;
 *  - cancel: pre-answer only, and it belongs to the initiator (a recipient declines its offer);
 *  - after termination an ex-participant may REPLAY a command with its original idempotency
 *    key (the backend answers the recorded result); no new command can succeed then.
 *
 * Dependency-free on purpose (node tests import it with --experimental-strip-types).
 */

export type ControlOperation = "cancel" | "end" | "hold" | "resume" | "transfer" | "leave" | "ivrInput";

export interface PermissionParticipant {
  participantId: string;
  principalId: string;
  role?: string;
  endedAt: string | null;
}

export interface PermissionSession {
  sessionId: string;
  participantId: string;
  endpointId?: string | null;
  endedAt?: string | null;
  removedAt?: string | null;
}

export interface PermissionCall {
  state: string;
  initiatorPrincipalId: string;
  participants: PermissionParticipant[];
  sessions?: PermissionSession[];
}

const TERMINAL = new Set(["ENDED", "DECLINED", "BUSY", "NO_ANSWER", "FAILED", "CANCELLED"]);
const PRE_ANSWER = new Set(["REQUESTED", "AUTHORIZED", "RESOLVING", "ROUTING", "DIALING", "RINGING"]);

export function isTerminalState(state: string): boolean {
  return TERMINAL.has(state);
}

/** Terminal or ENDING: only an idempotent replay of an earlier command can still be answered. */
export function isWindingDown(state: string): boolean {
  return state === "ENDING" || TERMINAL.has(state);
}

/** The principal's participant: the active one when it exists, else its latest historical one. */
export function ownParticipant(call: PermissionCall, principalId: string): PermissionParticipant | undefined {
  const mine = call.participants.filter((p) => p.principalId === principalId);
  return mine.find((p) => !p.endedAt) ?? mine.at(-1);
}

export function activeParticipant(call: PermissionCall, principalId: string): PermissionParticipant | undefined {
  return call.participants.find((p) => p.principalId === principalId && !p.endedAt);
}

/**
 * The adapter decision: may `principalId` send `operation` for this Call? It also allows an
 * ex-participant's replay once the Call is winding down (the backend refuses anything new).
 */
export function mayControl(call: PermissionCall, principalId: string, operation: ControlOperation): boolean {
  const mine = ownParticipant(call, principalId);
  if (!mine) return false;
  const active = !mine.endedAt;
  const replay = isWindingDown(call.state);
  switch (operation) {
    case "cancel":
      return call.initiatorPrincipalId === principalId && (PRE_ANSWER.has(call.state) || replay);
    case "end":
    case "hold":
    case "resume":
    case "transfer":
      return active || replay;
    case "ivrInput":
      // Enterprise IVR menus answer the caller's keypad only.
      return call.initiatorPrincipalId === principalId && active;
    case "leave":
      return true; // the session itself is checked against the participant by the caller
  }
}

/**
 * The UI decision: which controls to render for the signed-in user (no replays: a terminal
 * Call shows no controls). `available` is the server's `GET /v1/calls/{id}/actions` answer.
 */
export function visibleControls(
  call: PermissionCall,
  principalId: string,
  available: (action: string) => boolean,
): Record<"cancel" | "end" | "hold" | "resume" | "transfer", boolean> {
  const live = !isWindingDown(call.state);
  const active = !!activeParticipant(call, principalId);
  const show = (operation: "cancel" | "end" | "hold" | "resume" | "transfer") =>
    live && active && mayControl(call, principalId, operation) && available(operation.toUpperCase());
  return {
    cancel: show("cancel"),
    // Before the answer the initiator cancels; ending is for answered calls.
    end: show("end") && !(call.state === "REQUESTED"),
    hold: show("hold"),
    resume: show("resume"),
    transfer: show("transfer"),
  };
}

/** The signed-in user's own live session on this device (for leave / media join). */
export function ownSession(call: PermissionCall, principalId: string, endpointId?: string | null): PermissionSession | undefined {
  const mine = activeParticipant(call, principalId);
  if (!mine) return undefined;
  return (call.sessions ?? []).find((s) =>
    s.participantId === mine.participantId && !s.endedAt && !s.removedAt && (!endpointId || s.endpointId === endpointId));
}
