import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { qualityService } from "@/services/v2/quality.service";
import type { QualitySampleBatch } from "@/types/v2";

export const qualityKeys = {
  all: ["v2", "quality"] as const,
  conference: (conferenceId: string) =>
    [...qualityKeys.all, "conference", conferenceId] as const,
};

export function useConferenceQuality(conferenceId: string, enabled = true) {
  return useQuery({
    queryKey: qualityKeys.conference(conferenceId),
    queryFn: () => qualityService.getQuality(conferenceId),
    enabled: !!conferenceId && enabled,
    refetchInterval: enabled ? 30_000 : false,
  });
}

export function useSubmitQualitySamples(conferenceId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (batch: QualitySampleBatch) =>
      qualityService.submitSamples(conferenceId, batch),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: qualityKeys.conference(conferenceId),
      }),
  });
}
