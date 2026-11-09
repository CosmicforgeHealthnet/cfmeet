/**
 * Organization membership on mobile (ORG-016). Administration (members, roles, invitations,
 * API credentials, billing, calling) stays web-first; mobile covers the member's own view:
 * workspaces, memberships, the member list, invitation accept/decline and leaving.
 */
import { send } from "@/lib/api/client";
import { ROUTES } from "@/lib/api/routes";
import type {
  BusinessOrganizationMembership,
  MyOrganizationResponse,
  OrganizationInvitationTokenRequest,
  OrganizationMemberResponse,
  WorkspaceCapabilities,
  WorkspacePage,
} from "@/lib/api/generated/meeting-service";

export const organizationsApi = {
  workspaces: () => send<WorkspacePage>(ROUTES.myWorkspaces, [], { params: { limit: 100 } }),
  capabilities: (workspaceId: string) => send<WorkspaceCapabilities>(ROUTES.myWorkspaceCapabilities, [workspaceId]),
  myOrganizations: () => send<MyOrganizationResponse[]>(ROUTES.myOrganizations, []),
  members: (organizationId: string) => send<OrganizationMemberResponse[]>(ROUTES.organizationMembers, [organizationId]),
  /** The invitation token travels only in the request body (never in a URL or a log). */
  acceptInvitation: (token: string) =>
    send<Record<string, unknown>>(ROUTES.acceptOrganizationInvitation, [], {
      data: { token } satisfies OrganizationInvitationTokenRequest,
    }),
  declineInvitation: (token: string) =>
    send<Record<string, unknown>>(ROUTES.declineOrganizationInvitation, [], {
      data: { token } satisfies OrganizationInvitationTokenRequest,
    }),
  leave: (organizationId: string) =>
    send<BusinessOrganizationMembership>(ROUTES.leaveOrganization, [organizationId], { data: {} }),
};
