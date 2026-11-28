/**
 * The publishing queue's transport: the Messaging-backed Status API (ADR 0069,
 * messaging-transport.ts). The transitional fallback to the previous `/api/v2/social/*`
 * API was removed once #193 was deployed (ADR 0069 "Rollout" 1).
 */
import { createMessagingTransport } from "./messaging-transport";
import type { StatusTransport } from "./types";

export function createSocialTransport(): StatusTransport {
  return createMessagingTransport();
}
