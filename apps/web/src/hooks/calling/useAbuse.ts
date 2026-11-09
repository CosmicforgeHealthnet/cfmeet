"use client";

/**
 * React Query hooks for abuse reporting (CAL-005).
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { abuseService } from "@/services/calling";
import type { CreateAbuseReportRequest } from "@/types/calling";

export const abuseKeys = { mine: ["calling", "abuse-reports"] as const };

/** The reports the signed-in user filed (to show "already reported"). */
export function useMyAbuseReports(enabled = true) {
  return useQuery({ queryKey: abuseKeys.mine, queryFn: abuseService.mine, enabled, retry: false, staleTime: 30_000 });
}

export function useReportAbuse(callId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: CreateAbuseReportRequest) => abuseService.report(callId, data),
    retry: false,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: abuseKeys.mine }),
  });
}
