/**
 * Abuse reports (CAL-005) through the same-origin adapter: the reporter is the signed-in
 * participant; Calling derives the reported party.
 */

import callingApi from "./api";
import type { AbuseReport, AbuseReportList, CreateAbuseReportRequest } from "@/types/calling";

export const abuseService = {
  /** Idempotent per (call, reporter): a repeat answers the existing report. */
  report: async (callId: string, body: CreateAbuseReportRequest): Promise<AbuseReport> =>
    (await callingApi.post<AbuseReport>(`/v1/calls/${encodeURIComponent(callId)}/abuse-reports`, body)).data,

  mine: async (): Promise<AbuseReportList> =>
    (await callingApi.get<AbuseReportList>("/v1/abuse-reports")).data,
};
