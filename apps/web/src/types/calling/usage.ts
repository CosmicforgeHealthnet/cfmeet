/**
 * Usage & Limits types for the Calling API.
 * Tracks calling usage metrics and quota limits.
 */

import type { Uuid } from "./common";

/** Period for usage tracking */
export type LimitPeriod = "DAILY" | "WEEKLY" | "MONTHLY" | "BILLING_CYCLE";

/** Type of usage metric */
export type UsageMetric =
  | "OUTBOUND_MINUTES"
  | "INBOUND_MINUTES"
  | "RECORDINGS_MINUTES"
  | "RECORDINGS_STORAGE_GB"
  | "CONCURRENT_CALLS"
  | "SMS_MESSAGES";

/** Enforcement action when limit is reached */
export type EnforcementAction = "WARN" | "SOFT_LIMIT" | "HARD_LIMIT";

/** A single usage limit */
export interface UsageLimit {
  metric: UsageMetric;
  period: LimitPeriod;
  limit: number;
  used: number;
  remaining: number;
  resetsAt?: string;
  enforcementAction: EnforcementAction;
}

/** Current limit status for a principal */
export interface LimitStatus {
  principalId: Uuid;
  limits: UsageLimit[];
  asOf: string;
}

/** A single usage fact/metric */
export interface UsageFact {
  metric: UsageMetric;
  period: LimitPeriod;
  periodStart: string;
  periodEnd: string;
  value: number;
  unit: string;
}

/** List of usage facts */
export interface UsageList {
  principalId: Uuid;
  facts: UsageFact[];
}

/** Filters for querying usage */
export interface UsageFilters {
  period?: LimitPeriod;
  metrics?: UsageMetric[];
  from?: string;
  to?: string;
}
