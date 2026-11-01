"use client";

/**
 * React Query hooks for usage and limits management.
 */

import { useQuery } from "@tanstack/react-query";
import { usageService } from "@/services/calling";
import type { UsageFilters } from "@/types/calling";

export const usageKeys = {
  callingLimits: ["calling", "limits"] as const,
  callingUsage: (filters?: UsageFilters) => ["calling", "usage", filters] as const,
  platformLimits: ["platform", "limits"] as const,
  platformUsage: (filters?: UsageFilters) => ["platform", "usage", filters] as const,
};

/**
 * Get calling-specific limit status
 */
export function useCallingLimits() {
  return useQuery({
    queryKey: usageKeys.callingLimits,
    queryFn: () => usageService.getCallingLimits(),
    staleTime: 1000 * 60 * 5,
  });
}

/**
 * Get calling-specific usage facts
 */
export function useCallingUsage(filters: UsageFilters = {}) {
  return useQuery({
    queryKey: usageKeys.callingUsage(filters),
    queryFn: () => usageService.getCallingUsage(filters),
    staleTime: 1000 * 60 * 5,
  });
}

/**
 * Get general platform limit status
 */
export function usePlatformLimits() {
  return useQuery({
    queryKey: usageKeys.platformLimits,
    queryFn: () => usageService.getLimits(),
    staleTime: 1000 * 60 * 5,
  });
}

/**
 * Get general platform usage facts
 */
export function usePlatformUsage(filters: UsageFilters = {}) {
  return useQuery({
    queryKey: usageKeys.platformUsage(filters),
    queryFn: () => usageService.getUsage(filters),
    staleTime: 1000 * 60 * 5,
  });
}
