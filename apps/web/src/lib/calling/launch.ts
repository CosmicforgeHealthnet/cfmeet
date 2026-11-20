/**
 * A call started from a Call button elsewhere (Contacts, a chat): the Calls page shows the
 * calling screen at once (`announce`), then places the call as soon as this browser is a
 * connected calling device — unless the user cancelled it meanwhile.
 *
 * Owner report (2026-11-19, "calling a newly added contact keeps showing Calling…"): the
 * page decided "cancelled before it started" from the calling screen not being up yet in
 * the very render that put it up (React state is only visible on the next render), and it
 * forgot the launch once the address bar dropped `?contactId=`; so a call from Contacts was
 * never placed and the screen said "Calling…" forever. A newly added contact has no chat
 * yet, so Contacts is the only place to call her from — the path that never placed calls.
 *
 * This decision is a pure function of what the page has seen so the rule is testable:
 *  - nothing pending: wait;
 *  - the calling screen has not been seen up yet: wait (it is being put up now);
 *  - it was up and is gone again: the user cancelled: drop the launch;
 *  - it is up: place the call once this browser is a calling device and not in a call.
 *
 * Dependency-free on purpose (node tests import it with --experimental-strip-types).
 */
export type LaunchStep = "wait" | "place" | "drop";

export interface LaunchState {
  /** A launch is remembered and not placed yet. */
  pending: boolean;
  /** The calling screen is up (`placing`) in this render. */
  placing: boolean;
  /** The calling screen was seen up in an earlier render. */
  sawPlacing: boolean;
  /** This browser is a registered calling device. */
  endpointReady: boolean;
  /** This browser already carries a call's media. */
  inCall: boolean;
}

export function launchStep(state: LaunchState): { step: LaunchStep; sawPlacing: boolean } {
  const sawPlacing = state.sawPlacing || state.placing;
  if (!state.pending) return { step: "wait", sawPlacing };
  if (!state.placing) return { step: state.sawPlacing ? "drop" : "wait", sawPlacing };
  if (!state.endpointReady || state.inCall) return { step: "wait", sawPlacing };
  return { step: "place", sawPlacing };
}

/**
 * CF1 ("on another call", WhatsApp): what the caller's calling screen says when the call
 * it just placed is already over. BUSY: everyone called is already on another call, so
 * nobody was rung — "<name> is on another call". Any other end: "" (the screen closes and
 * the call's own details say why).
 */
export function endedCallText(state: string, label: string | null): string {
  if (state !== "BUSY") return "";
  const name = label?.trim();
  return name ? `${name} is on another call` : "They're on another call";
}
