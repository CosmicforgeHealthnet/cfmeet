/**
 * Safety & Abuse API service.
 * Handles abuse reporting for unwanted or abusive calls.
 */

import callingApi from "./api";
import type {
  CreateAbuseReportRequest,
  AbuseReportResult,
} from "@/types/calling";

export const abuseService = {
  /**
   * Report an unwanted or abusive call
   */
  report: async (
    callId: string,
    body: CreateAbuseReportRequest
  ): Promise<AbuseReportResult> => {
    const response = await callingApi.post<AbuseReportResult>(
      `/v1/calls/${encodeURIComponent(callId)}/abuse-reports`,
      body
    );
    return response.data;
  },
};
