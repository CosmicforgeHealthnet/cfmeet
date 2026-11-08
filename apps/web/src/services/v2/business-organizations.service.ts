/**
 * First-party business organizations (Blueprint Sections 4, 5, 8, 17): the routes Meeting
 * Service relays to the authoritative Organizations Service. Every path and body below is
 * the served contract (packages/contracts/openapi/meeting-service.json, generated types).
 *
 * Contract gaps (recorded in docs/frontend/section-17/web-account-organizations.md): the
 * invitation list, audit events, provisioning, invitation accept/decline and ownership
 * transfer responses are declared as a bare `object` in OpenAPI. The minimal shapes below
 * are read from the Organizations Service handlers, and the UI treats every field
 * defensively; they should be replaced by generated types once the contract names them.
 */
import apiV2 from "./api";
import type {
  BusinessOrganization,
  BusinessOrganizationMembership,
  CreateBusinessOrganizationRequest,
  CreateOrganizationInvitationRequest,
  OrganizationInvitation,
  OrganizationInvitationTokenRequest,
  OrganizationLifecycleRequest,
  OrganizationMemberResponse,
  OrganizationOwnershipTransferRequest,
  UpdateBusinessMembershipRequest,
  UpdateBusinessOrganizationRequest,
} from "@/types/generated/meeting-service";

/** `GET /organizations/{id}/invitations` → `{ items }` (contract: object). */
export interface OrganizationInvitationList {
  items: OrganizationInvitation[];
}

/** One `GET /organizations/{id}/audit-events` item (contract: object). */
export interface OrganizationAuditEvent {
  id: string;
  seq: number;
  action: string;
  actor?: { kind?: string | null; userId?: string | null; serviceId?: string | null } | null;
  targetType?: string | null;
  targetId?: string | null;
  summary?: unknown;
  requestId?: string | null;
  occurredAt: string;
}

export interface OrganizationAuditPage {
  items: OrganizationAuditEvent[];
  /** The `seq` to pass as `before` for the next (older) page; null at the end. */
  nextCursor?: number | null;
}

/** `GET /organizations/{id}/provisioning` (contract: object). */
export interface OrganizationProvisioning {
  organizationId: string;
  provisioningState: string;
  provisioningEnabled?: boolean;
  workflowState?: string | null;
  step?: string | null;
  attempts?: number;
  lastErrorClass?: string | null;
  nextAttemptAt?: string | null;
  updatedAt?: string | null;
}

const org = (id: string) => `/organizations/${encodeURIComponent(id)}`;

export const businessOrganizationsService = {
  /** `POST /organizations` — the caller becomes OWNER. `idempotencyKey` makes retries safe. */
  create: async (body: CreateBusinessOrganizationRequest, idempotencyKey: string) =>
    (await apiV2.post<BusinessOrganization>("/organizations", body, { headers: { "Idempotency-Key": idempotencyKey } })).data,

  get: async (id: string) => (await apiV2.get<BusinessOrganization>(org(id))).data,

  /** Optimistic: `expectedVersion` must be the current `version` (409 VERSION_CONFLICT). */
  update: async (id: string, body: UpdateBusinessOrganizationRequest) =>
    (await apiV2.patch<BusinessOrganization>(org(id), body)).data,

  provisioning: async (id: string) => (await apiV2.get<OrganizationProvisioning>(`${org(id)}/provisioning`)).data,

  /** Served as an array without pagination (ORG-006 gap: no display names, no cursor). */
  members: async (id: string) => (await apiV2.get<OrganizationMemberResponse[]>(`${org(id)}/members`)).data,

  /** The path segment is the membership `id` from the member list. */
  updateMembership: async (id: string, membershipId: string, body: UpdateBusinessMembershipRequest) =>
    (await apiV2.patch<BusinessOrganizationMembership>(`${org(id)}/members/${encodeURIComponent(membershipId)}`, body)).data,

  leave: async (id: string) =>
    (await apiV2.post<BusinessOrganizationMembership>(`${org(id)}/members/me/leave`, {})).data,

  transferOwnership: async (id: string, body: OrganizationOwnershipTransferRequest) =>
    (await apiV2.post<Record<string, unknown>>(`${org(id)}/ownership-transfer`, body)).data,

  invitations: async (id: string, status?: string) =>
    (await apiV2.get<OrganizationInvitationList>(`${org(id)}/invitations`, { params: status ? { status } : undefined })).data
      .items ?? [],

  /** The answer carries the one-time `token` (shown once, never stored by the client). */
  invite: async (id: string, body: CreateOrganizationInvitationRequest, idempotencyKey: string) =>
    (
      await apiV2.post<OrganizationInvitation>(`${org(id)}/invitations`, body, {
        headers: { "Idempotency-Key": idempotencyKey },
      })
    ).data,

  revokeInvitation: async (id: string, invitationId: string) =>
    (await apiV2.post<OrganizationInvitation>(`${org(id)}/invitations/${encodeURIComponent(invitationId)}/revoke`, {})).data,

  /** Token in the body only — never in a URL, query or log. */
  acceptInvitation: async (token: string) => {
    const body: OrganizationInvitationTokenRequest = { token };
    return (await apiV2.post<Record<string, unknown>>("/organization-invitations/accept", body)).data;
  },

  declineInvitation: async (token: string) => {
    const body: OrganizationInvitationTokenRequest = { token };
    return (await apiV2.post<Record<string, unknown>>("/organization-invitations/decline", body)).data;
  },

  lifecycle: async (id: string, action: "suspend" | "reactivate" | "archive", expectedVersion: number) => {
    const body: OrganizationLifecycleRequest = { expectedVersion };
    return (await apiV2.post<BusinessOrganization>(`${org(id)}/${action}`, body)).data;
  },

  auditEvents: async (id: string, before?: number | null, limit = 50) =>
    (
      await apiV2.get<OrganizationAuditPage>(`${org(id)}/audit-events`, {
        params: { limit, ...(typeof before === "number" ? { before } : {}) },
      })
    ).data,
};
