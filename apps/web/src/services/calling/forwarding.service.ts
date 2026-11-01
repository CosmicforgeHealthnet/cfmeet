/**
 * Call Forwarding API service.
 * Manages call forwarding rules and destinations.
 */

import callingApi from "./api";
import type {
  ForwardingRule,
  ForwardingRuleList,
  CreateForwardingRuleRequest,
  UpdateForwardingRuleRequest,
} from "@/types/calling";

export const forwardingService = {
  /**
   * List all forwarding rules
   */
  list: async (): Promise<ForwardingRuleList> => {
    const response = await callingApi.get<ForwardingRuleList>(
      "/v1/enterprise/forwarding-rules"
    );
    return response.data;
  },

  /**
   * Get a specific forwarding rule
   */
  get: async (ruleId: string): Promise<ForwardingRule> => {
    const response = await callingApi.get<ForwardingRule>(
      `/v1/enterprise/forwarding-rules/${encodeURIComponent(ruleId)}`
    );
    return response.data;
  },

  /**
   * Create a new forwarding rule
   */
  create: async (body: CreateForwardingRuleRequest): Promise<ForwardingRule> => {
    const response = await callingApi.post<ForwardingRule>(
      "/v1/enterprise/forwarding-rules",
      body
    );
    return response.data;
  },

  /**
   * Update a forwarding rule's target or settings
   */
  update: async (
    ruleId: string,
    body: UpdateForwardingRuleRequest
  ): Promise<ForwardingRule> => {
    const response = await callingApi.patch<ForwardingRule>(
      `/v1/enterprise/forwarding-rules/${encodeURIComponent(ruleId)}`,
      body
    );
    return response.data;
  },

  /**
   * Delete (disable) a forwarding rule
   */
  delete: async (ruleId: string): Promise<void> => {
    await callingApi.delete(
      `/v1/enterprise/forwarding-rules/${encodeURIComponent(ruleId)}`
    );
  },
};
