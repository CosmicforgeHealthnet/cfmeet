/**
 * Incoming-call presentation rules (CAL-004). Dependency-free (node tests).
 *
 * The recovered ring offer (`GET /v1/calling/invitations`) carries no caller identity; the
 * callee is a participant of the Call, so the caller is read from the Call view
 * (`participants[role=INITIATOR].displayName`) through the adapter and, when Calling holds
 * no display name, from Meeting's `POST /api/v2/messaging/principals/names` (colleagues only).
 */

export interface RingOffer {
  invitationId: string;
  callId: string;
  callKind: string;
  purpose: string;
  actionable: boolean;
  expiresAt: string;
}

/** Offers that should ring now: actionable and unexpired, oldest first, one per Call. */
export function ringingOffers<T extends RingOffer>(offers: T[], now: number, dismissed: ReadonlySet<string> = new Set()): T[] {
  const seen = new Set<string>();
  return offers
    .filter((o) => o.actionable && !dismissed.has(o.invitationId) && Date.parse(o.expiresAt) > now)
    .sort((a, b) => Date.parse(a.expiresAt) - Date.parse(b.expiresAt))
    .filter((o) => (seen.has(o.callId) ? false : (seen.add(o.callId), true)));
}

export interface CallerParticipant {
  principalId: string;
  role?: string;
  displayName: string | null;
}

/** The principal whose identity the callee sees: the initiator (for a transfer, still the call's caller). */
export function callerPrincipal(participants: CallerParticipant[], selfPrincipalId: string): CallerParticipant | undefined {
  return participants.find((p) => p.role === "INITIATOR" && p.principalId !== selfPrincipalId)
    ?? participants.find((p) => p.principalId !== selfPrincipalId);
}

export function callerLabel(
  participants: CallerParticipant[] | undefined,
  selfPrincipalId: string,
  names: Readonly<Record<string, string>> = {},
): string {
  if (!participants) return "Incoming call";
  const caller = callerPrincipal(participants, selfPrincipalId);
  if (!caller) return "CF Meet user";
  const name = caller.displayName?.trim() || names[caller.principalId]?.trim();
  return name || "CF Meet user";
}

export function offerHeadline(offer: Pick<RingOffer, "callKind" | "purpose">): string {
  const kind = offer.callKind === "VIDEO" ? "video" : "voice";
  return offer.purpose === "TRANSFER" ? `Transferred ${kind} call` : `Incoming ${kind} call`;
}
