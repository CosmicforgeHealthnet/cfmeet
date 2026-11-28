/**
 * The publishing queue's transport: Messaging (ADR 0069, messaging-transport.ts), with a
 * TRANSITIONAL fallback to the previous `/api/v2/social/*` API (legacy-transport.ts) while the
 * backend still runs the release before #193 — only on the Meeting relay's route-not-allowed
 * answer (social-backend.ts), remembered for the session.
 * TODO(#193 / ADR 0069 "Rollout"): after #193 is deployed, return createMessagingTransport()
 * here and delete legacy-transport.ts.
 *
 * An upload stays on the backend that started it; capabilities follow the current backend
 * (the legacy one keeps its degraded flags, so the queue uses findCreatedStatus there).
 */
import { socialBackend } from "@/services/v2/social-backend";
import { createLegacyTransport } from "./legacy-transport";
import { createMessagingTransport } from "./messaging-transport";
import { createSwitchingTransport } from "./switching-transport";
import type { StatusTransport } from "./types";

export function createSocialTransport(): StatusTransport {
  return createSwitchingTransport(createMessagingTransport(), createLegacyTransport(), socialBackend);
}
