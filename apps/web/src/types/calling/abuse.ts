/**
 * Abuse reports. Mirrors services/calling/src/calls/app/abuse_reports.rs (the served contract
 * types the body as a free object): a bounded category and an optional bounded comment CODE,
 * never free text. The adapter injects the reporter principal (CAL-005); the reported principal
 * is derived by Calling.
 */

import type { Uuid } from "./common";

export type AbuseCategory = "UNWANTED" | "HARASSMENT" | "SPAM" | "FRAUD_SUSPECTED" | "OTHER";

export interface CreateAbuseReportRequest {
  category: AbuseCategory;
  commentCode?: string;
}

export interface AbuseReport {
  abuseReportId: Uuid;
  callId: Uuid;
  principalId: Uuid;
  reportedPrincipalId: Uuid | null;
  category: AbuseCategory;
  commentCode: string | null;
  status: string;
  createdAt: string;
  reviewedAt: string | null;
  version: number;
}

export interface AbuseReportList {
  items: AbuseReport[];
}
