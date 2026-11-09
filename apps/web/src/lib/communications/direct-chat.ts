/**
 * Contact / colleague → conversation navigation. Opens (or returns — Messaging answers the
 * existing live DIRECT conversation of a pair) a MANAGED direct chat through the Meeting relay.
 * MANAGED is server-readable and the caller's UI says so before calling this; PRIVATE_E2EE
 * stays gated and is never chosen here.
 */
import { messagingService, relay } from "@/services/v2/messaging.service";

type Created = { conversationId: string; securityProfile: string };

function checked(c: Created): string {
  // Never accept a different profile than the one requested.
  if (c.securityProfile !== "MANAGED") throw new Error("PROFILE_NOT_GRANTED");
  return c.conversationId;
}

/** A connected contact, by CF Meet ID (Messaging resolves it through the Hub policy check). */
export async function openDirectChatWithContact(cfMeetId: string): Promise<string> {
  const c = await relay<Created>(
    "POST",
    "v1/conversations",
    { contextClass: "DIRECT", counterpartCfMeetId: cfMeetId, securityProfile: "MANAGED", managedDisclosureAcknowledged: true },
    `web-${crypto.randomUUID()}`,
  );
  return checked(c);
}

/** A colleague of the selected organization, by account user id. */
export async function openDirectChatWithColleague(userId: string): Promise<string> {
  const [person] = (await messagingService.resolve([userId])).people;
  if (!person) throw new Error("NOT_FOUND");
  const c = await relay<Created>(
    "POST",
    "v1/conversations",
    { contextClass: "DIRECT", counterpartPrincipalId: person.principalId, securityProfile: "MANAGED", managedDisclosureAcknowledged: true },
    `web-${crypto.randomUUID()}`,
  );
  return checked(c);
}
