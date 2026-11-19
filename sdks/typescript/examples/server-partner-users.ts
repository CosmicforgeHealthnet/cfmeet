// Server-side (your backend): partner users, user tokens and server-placed calls — the
// KarryGo example (a rider calls the customer about an active delivery; chat is the
// fallback when nobody answers).
//
//   CFMEET_API_CREDENTIAL=cfmk_sbx_... node karrygo-server.js
//
// The platform credential needs principals.read / principals.write / user_tokens.issue
// (users and tokens), plus calls.write, messages.read / messages.send and meetings.write
// for the grants you mint. It NEVER leaves this server: devices only ever get a short-lived
// user token (cfut_...) minted for the signed-in user.

import { CfMeetApiError, CfMeetClient, UnknownOutcomeError, type UserTokenGrantsRequest } from '@cfmeet/platform-sdk';

const cfmeet = new CfMeetClient({
  baseUrl: process.env.CFMEET_API_URL ?? 'https://api.cfmeet.com',
  auth: { type: 'platformCredential', credential: process.env.CFMEET_API_CREDENTIAL ?? '' },
});

/** Your own user record (KarryGo's database). */
interface AppUser {
  readonly id: string; // KarryGo's opaque user id — never a phone number
  readonly name: string;
  cfmeetUserId?: string;
}

/**
 * `GET /karrygo/cfmeet-token` (behind KarryGo's own login): the device's tokenProvider
 * calls this. One call creates the CF Meet user on first use and mints a 1-hour token.
 */
export async function cfmeetTokenFor(user: AppUser): Promise<{ token: string; expiresAt: string }> {
  const grants: UserTokenGrantsRequest = { msg: ['read', 'send'], call: ['receive'] };
  const minted = await cfmeet.users.upsertWithToken({ externalId: user.id, displayName: user.name, grants, ttlSeconds: 3600 });
  user.cfmeetUserId = minted.user.id; // keep it: calls and conversations address principals
  return { token: minted.token, expiresAt: minted.expiresAt };
}

/** A meeting token: the same, with a grant for one conference. */
export async function meetingTokenFor(user: AppUser, conferenceId: string): Promise<string> {
  const minted = await cfmeet.users.upsertWithToken({
    externalId: user.id,
    displayName: user.name,
    grants: { mtg: [{ conferenceId, role: 'PARTICIPANT' }] },
  });
  return minted.token;
}

/** The user signed out everywhere / was banned: every device credential ends. */
export async function signOutEverywhere(user: AppUser, suspend = false): Promise<void> {
  if (!user.cfmeetUserId) return;
  if (suspend) await cfmeet.users.update(user.cfmeetUserId, { status: 'SUSPENDED' });
  else await cfmeet.users.revokeSessions(user.cfmeetUserId);
}

/** Delivery state in KarryGo's own database. */
interface Delivery {
  readonly id: string;
  readonly active: boolean;
  readonly rider: AppUser;
  readonly customer: AppUser;
  /** A per-delivery idempotency key for the call, stored with the delivery before calling. */
  readonly callKey: string;
}

/**
 * `POST /karrygo/deliveries/{id}/call` from the rider's app, with the rider's call
 * endpoint (`calls.endpointId` on the device). KarryGo decides who may call whom: only
 * the delivery's rider, only while it is active. Calls are server-placed.
 */
export async function riderCallsCustomer(delivery: Delivery, riderEndpointId: string): Promise<{ callId: string } | { fallback: 'chat'; conversationId: string }> {
  if (!delivery.active) throw new Error('delivery not active');
  const rider = delivery.rider.cfmeetUserId;
  const customer = delivery.customer.cfmeetUserId;
  if (!rider || !customer) throw new Error('both users must have signed in to CF Meet once');
  try {
    const res = await cfmeet.calling.createCall({
      body: { kind: 'VOICE', initiatorPrincipalId: rider, initiatorEndpointId: riderEndpointId, targetPrincipalIds: [customer], initiatorDisplayName: delivery.rider.name },
      idempotencyKey: delivery.callKey, // paid class: never retried automatically
    });
    if (res.data.liveInvitationCount > 0) return { callId: res.data.call.callId };
  } catch (e) {
    // An unknown outcome: replay with the SAME key later (never a new call blindly).
    if (e instanceof UnknownOutcomeError) throw e;
    if (!(e instanceof CfMeetApiError)) throw e;
  }
  // Nobody can be rung (no app open, no push): fall back to the delivery chat.
  return { fallback: 'chat', conversationId: await deliveryConversation(delivery) };
}

/** The delivery's MANAGED chat between rider and customer (created by the rider). */
export async function deliveryConversation(delivery: Delivery): Promise<string> {
  const res = await cfmeet.messaging.createConversation({
    headers: { 'cf-acting-principal': delivery.rider.cfmeetUserId ?? '' },
    body: { contextClass: 'TRANSACTION', counterpartPrincipalId: delivery.customer.cfmeetUserId, title: `Delivery ${delivery.id}` },
    idempotencyKey: `delivery-chat-${delivery.id}`,
  });
  return res.data.conversationId;
}
