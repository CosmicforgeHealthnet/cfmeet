"use client";

import { useQuery } from "@tanstack/react-query";
import { messagingService } from "@/services/v2/messaging.service";

/**
 * Display names of Calling principals through Meeting's
 * `POST /api/v2/messaging/principals/names` (own principal and verified-organization
 * colleagues only; anyone else is simply left out). Used where Calling carries no name
 * (recovered ring offers, voicemail callers).
 */
export function usePrincipalNames(principalIds: Array<string | null | undefined>) {
  const ids = [...new Set(principalIds.filter((id): id is string => !!id))].sort().slice(0, 256);
  return useQuery({
    queryKey: ["calling", "principal-names", ids],
    queryFn: async () => {
      const answer = await messagingService.names(ids);
      return Object.fromEntries(answer.principals.map((p) => [p.principalId, p.displayName])) as Record<string, string>;
    },
    enabled: ids.length > 0,
    staleTime: 5 * 60_000,
    retry: false,
  });
}
