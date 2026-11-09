import { QueryClient } from "@tanstack/react-query";
import { decodeApiError } from "@/lib/api/errors";

/** Retry only failures a retry can fix (network, 5xx, 429) — never 4xx answers. */
function shouldRetry(failureCount: number, error: unknown): boolean {
  const { kind } = decodeApiError(error);
  return failureCount < 2 && (kind === "network" || kind === "unavailable" || kind === "rate_limited");
}

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 60 * 1000,
      retry: shouldRetry,
      refetchOnWindowFocus: false,
    },
    mutations: {
      retry: 0,
    },
  },
});
