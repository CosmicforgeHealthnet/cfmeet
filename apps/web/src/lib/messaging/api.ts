/**
 * CF Meet web client → Meeting Service → Messaging (Part V §104).
 *
 * Every Messaging operation goes through the Meeting Service, which verifies the CF Meet
 * login and acts for the account's own Messaging principal (signed into its service
 * token): `/api/v2/messaging/relay/v1/...` for the allowlisted end-user operations and
 * `/api/v2/messaging/{me,directory,...}` for identity. The browser never holds a
 * Messaging or Platform Access credential; the only Messaging token it ever sees is the
 * short-lived, device-bound client token for the realtime gateway.
 *
 * Requests carry the login bearer only (never a meeting participant token), so the
 * relay cannot confuse a guest identity with the signed-in account.
 */
// Compatibility exports for the MLS engine and existing callers. The v2 service
// owns the HTTP contract; cryptographic state stays in the dedicated worker.
import { MessagingApiError } from "./errors";
export { messagingService as identityApi, relay } from "@/services/v2/messaging.service";
export { MESSAGING_V2_BASE_URL as V2_BASE_URL } from "@/services/v2/messaging-http";
export { MessagingApiError };
export type { MessagingIdentity, Colleague } from "@/types/messaging";

export const b64u = {
  enc(b: Uint8Array): string {
    let s = "";
    for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
    return btoa(s).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  },
  dec(s: string): Uint8Array {
    if (!/^[A-Za-z0-9_-]*$/.test(s)) throw new MessagingApiError(0, "BAD_BASE64", null);
    const bin = atob(s.replaceAll("-", "+").replaceAll("_", "/") + "===".slice((s.length + 3) % 4));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  },
};
