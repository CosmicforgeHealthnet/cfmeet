/**
 * Per-organization Enterprise Calling administration through Meeting's relay
 * (Blueprint Section 12 follow-up, ADR 0058 A1):
 *
 *   GET|POST|PUT|PATCH|DELETE /api/v2/organizations/{id}/calling/{path}
 *   (operationIds v2_organization_calling_{get,post,put,patch,delete}, packages/contracts/openapi/meeting-service.json)
 *
 * Meeting verifies the human, requires `calling.enterprise.manage` + `features.calling` in the
 * organization, resolves the organization's own Calling scope and forwards with the member as
 * acting principal (audited as `actorPrincipalId`). `{path}` is the Calling path without the
 * leading slash and must match Meeting's allowlist exactly (lowercase canonical UUIDs).
 *
 * Answers (Section 12 §2): Calling 2xx/4xx relayed unchanged; `409 ORGANIZATION_CALLING_DISABLED`
 * (relay off — the default — no binding, or principal refused); `409 PROVISIONING_PENDING`;
 * `403 PERMISSION_DENIED` / `ORGANIZATION_POLICY_RESTRICTED`; `503 SERVICE_UNAVAILABLE`.
 */

import axios from "axios";
import { meetingCallingApi } from "./meeting-api";
import { newIdempotencyKey } from "@/lib/calling/idempotency";

export type OrganizationCallingMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

export type OrganizationCallingUnavailable =
  | "NOT_ENABLED"
  | "PROVISIONING_PENDING"
  | "FORBIDDEN"
  | "SUSPENDED"
  | "UNAVAILABLE";

const RELAYED_PATH = /^v1\/(enterprise\/[a-z0-9/-]+|calling\/(usage|limits)|webhooks(\/[a-z0-9/-]+)?|webhook-deliveries\/[0-9a-f-]{36}\/replay)$/;

export const organizationCallingService = {
  request: async <T>(organizationId: string, method: OrganizationCallingMethod, path: string, options: { params?: Record<string, string | number | undefined>; body?: unknown } = {}): Promise<T> => {
    if (!RELAYED_PATH.test(path)) throw new Error("This organization calling operation is not available.");
    const response = await meetingCallingApi.request<T>({
      method,
      url: `/organizations/${encodeURIComponent(organizationId)}/calling/${path}`,
      params: options.params,
      data: options.body,
      headers: method === "GET" ? undefined : { "Idempotency-Key": newIdempotencyKey() },
    });
    return response.data;
  },
};

/** Maps a relay failure to a truthful "why this view is not available" state (null: an ordinary error). */
export function organizationCallingUnavailable(error: unknown): OrganizationCallingUnavailable | null {
  if (!axios.isAxiosError<{ error?: { code?: string } }>(error)) return null;
  const status = error.response?.status;
  const code = error.response?.data?.error?.code;
  if (code === "ORGANIZATION_CALLING_DISABLED") return "NOT_ENABLED";
  if (code === "PROVISIONING_PENDING") return "PROVISIONING_PENDING";
  if (code === "ORGANIZATION_SUSPENDED") return "SUSPENDED";
  if (code === "PERMISSION_DENIED" || code === "ORGANIZATION_POLICY_RESTRICTED" || status === 403) return "FORBIDDEN";
  if (status === 503 || status === undefined) return "UNAVAILABLE";
  return null;
}
