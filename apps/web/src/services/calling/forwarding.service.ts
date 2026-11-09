/**
 * Self-service call forwarding (CAL-006) through the same-origin adapter, which binds the
 * rules to the signed-in principal and verifies ownership before changes.
 */

import callingApi from "./api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import type {
  CreateForwardingRuleRequest,
  ForwardingRule,
  ForwardingRuleList,
  UpdateForwardingRuleRequest,
} from "@/types/calling";

const mutation = () => ({ headers: { "Idempotency-Key": newIdempotencyKey() } });

export const forwardingService = {
  list: async (): Promise<ForwardingRuleList> =>
    (await callingApi.get<ForwardingRuleList>("/v1/enterprise/forwarding-rules", { params: { status: "ACTIVE" } })).data,

  create: async (body: CreateForwardingRuleRequest): Promise<ForwardingRule> =>
    (await callingApi.post<ForwardingRule>("/v1/enterprise/forwarding-rules", body, mutation())).data,

  update: async (ruleId: string, body: UpdateForwardingRuleRequest): Promise<ForwardingRule> =>
    (await callingApi.patch<ForwardingRule>(`/v1/enterprise/forwarding-rules/${encodeURIComponent(ruleId)}`, body, mutation())).data,

  /** Disables the rule (Calling keeps it as DISABLED). */
  remove: async (ruleId: string, expectedVersion: number): Promise<void> => {
    await callingApi.delete(`/v1/enterprise/forwarding-rules/${encodeURIComponent(ruleId)}`, {
      params: { expectedVersion }, ...mutation(),
    });
  },
};
