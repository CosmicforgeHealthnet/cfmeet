"use client";

/**
 * React Query hooks for abuse reporting.
 */

import { useMutation } from "@tanstack/react-query";
import { abuseService } from "@/services/calling";
import type { CreateAbuseReportRequest } from "@/types/calling";

/**
 * Report an unwanted or abusive call
 */
export function useReportAbuse(callId: string) {
  return useMutation({
    mutationFn: (data: CreateAbuseReportRequest) =>
      abuseService.report(callId, data),
  });
}
