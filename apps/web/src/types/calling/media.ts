/**
 * Media join credentials handed to a session's connection to the Call's media resource.
 * Mirrors packages/contracts/calling/control/v1.schema.json.
 */

import type { Uuid } from "./common";

export interface CallMediaCapabilities {
  canPublishAudio: boolean;
  canPublishVideo: boolean;
  canPublishData: boolean;
  canSubscribe: boolean;
}

export interface MediaCredential {
  sessionId: Uuid;
  url: string;
  token: string;
  expiresAt: string;
  capabilities: CallMediaCapabilities;
}
