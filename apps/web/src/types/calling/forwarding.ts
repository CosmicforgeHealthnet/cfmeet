/**
 * Call Forwarding types for the Calling API.
 * Manages call forwarding rules and destinations.
 */

import type { Uuid } from "./common";

/** Condition that triggers forwarding */
export type ForwardingCondition =
  | "ALWAYS"
  | "BUSY"
  | "NO_ANSWER"
  | "NOT_REACHABLE"
  | "OUTSIDE_HOURS";

/** Type of forwarding target */
export type ForwardingTargetType =
  | "EXTENSION"
  | "EXTERNAL_NUMBER"
  | "VOICEMAIL"
  | "PRINCIPAL";

/** Forwarding target destination */
export interface ForwardingTarget {
  type: ForwardingTargetType;
  extension?: string;
  externalNumber?: string;
  voicemailBoxId?: Uuid;
  principalId?: Uuid;
}

/** A call forwarding rule */
export interface ForwardingRule {
  forwardingRuleId: Uuid;
  principalId: Uuid;
  condition: ForwardingCondition;
  priority: number;
  target: ForwardingTarget;
  ringSeconds?: number;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

/** List of forwarding rules */
export interface ForwardingRuleList {
  data: ForwardingRule[];
}

/** Request body for creating a forwarding rule */
export interface CreateForwardingRuleRequest {
  condition: ForwardingCondition;
  target: ForwardingTarget;
  priority?: number;
  ringSeconds?: number;
}

/** Request body for updating a forwarding rule */
export interface UpdateForwardingRuleRequest {
  target?: ForwardingTarget;
  priority?: number;
  ringSeconds?: number;
  enabled?: boolean;
}
