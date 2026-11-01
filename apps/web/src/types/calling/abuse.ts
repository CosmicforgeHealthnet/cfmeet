/**
 * Safety & Abuse types for the Calling API.
 * Handles abuse reporting for unwanted or abusive calls.
 */

import type { Uuid } from "./common";

/** Category of abuse being reported */
export type AbuseCategory =
  | "HARASSMENT"
  | "SPAM"
  | "SCAM"
  | "THREAT"
  | "INAPPROPRIATE_CONTENT"
  | "IMPERSONATION"
  | "OTHER";

/** Status of an abuse report */
export type AbuseReportStatus = "SUBMITTED" | "UNDER_REVIEW" | "RESOLVED";

/** Request body for creating an abuse report */
export interface CreateAbuseReportRequest {
  category: AbuseCategory;
  description?: string;
  evidenceIds?: Uuid[];
}

/** Result of submitting an abuse report */
export interface AbuseReportResult {
  reportId: Uuid;
  callId: Uuid;
  category: AbuseCategory;
  status: AbuseReportStatus;
  submittedAt: string;
}
