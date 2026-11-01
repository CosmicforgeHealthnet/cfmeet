/**
 * Usage & Limits API service.
 * Tracks calling usage metrics and quota limits.
 */

import callingApi from "./api";
import type { LimitStatus, UsageList, UsageFilters } from "@/types/calling";

export const usageService = {
  /**
   * Get calling-specific limit status
   */
  getCallingLimits: async (): Promise<LimitStatus> => {
    const response = await callingApi.get<LimitStatus>("/v1/calling/limits");
    return response.data;
  },

  /**
   * Get calling-specific usage facts
   */
  getCallingUsage: async (filters: UsageFilters = {}): Promise<UsageList> => {
    const response = await callingApi.get<UsageList>("/v1/calling/usage", {
      params: filters,
    });
    return response.data;
  },

  /**
   * Get general platform limit status
   */
  getLimits: async (): Promise<LimitStatus> => {
    const response = await callingApi.get<LimitStatus>("/v1/limits");
    return response.data;
  },

  /**
   * Get general platform usage facts
   */
  getUsage: async (filters: UsageFilters = {}): Promise<UsageList> => {
    const response = await callingApi.get<UsageList>("/v1/usage", {
      params: filters,
    });
    return response.data;
  },
};
