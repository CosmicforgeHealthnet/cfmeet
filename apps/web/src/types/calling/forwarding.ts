/**
 * Personal call forwarding. Mirrors packages/contracts/calling/enterprise/v1.schema.json
 * (ForwardingRule, CreateForwardingRuleRequest, UpdateForwardingRuleRequest). The adapter binds
 * `principalId` to the signed-in principal and allows PRINCIPAL / EXTENSION / VOICEMAIL
 * destinations only (CAL-006). Destinations are typed ids, never telephone numbers.
 */

import type { Uuid } from "./common";

export type ForwardingCondition = "ALWAYS" | "BUSY" | "NO_ANSWER" | "UNAVAILABLE";

export type DestinationType =
  | "ENTERPRISE_MEMBER" | "PRINCIPAL" | "EXTENSION" | "RING_GROUP" | "QUEUE" | "IVR"
  | "VOICEMAIL" | "SCHEDULE" | "PUBLIC_CONTACT";

/** The destinations an end user may choose on the personal surface. */
export type SelfForwardingDestinationType = "PRINCIPAL" | "EXTENSION" | "VOICEMAIL";

export interface Destination {
  type: DestinationType;
  id: Uuid;
}

export interface ForwardingRule {
  forwardingRuleId: Uuid;
  principalId: Uuid;
  condition: ForwardingCondition;
  destination: Destination | null;
  status: "ACTIVE" | "DISABLED";
  version: number;
  createdAt: string;
  updatedAt: string;
  disabledAt: string | null;
}

export interface ForwardingRuleList {
  forwardingRules: ForwardingRule[];
  nextCursor: string | null;
}

/** principalId is injected by the adapter. */
export interface CreateForwardingRuleRequest {
  condition: ForwardingCondition;
  destination: { type: SelfForwardingDestinationType; id: Uuid };
}

export interface UpdateForwardingRuleRequest {
  expectedVersion: number;
  destination: { type: SelfForwardingDestinationType; id: Uuid };
}
