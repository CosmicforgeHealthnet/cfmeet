/**
 * Signed-in account operations (Blueprint Sections 2, 9 and the Sections 2–3 hardening):
 * profile, sessions, password change and self-deactivation.
 */
import api from "./api";
import apiV2 from "./v2/api";
import type {
  AccountProfile,
  AccountSessionListResponse,
  AccountSessionRevokeResponse,
  ChangePasswordRequest,
  DeactivateAccountRequest,
  MessageResponse,
  UpdateAccountProfileRequest,
} from "@/types/generated/meeting-service";

export const accountService = {
  getProfile: async () => (await apiV2.get<AccountProfile>("/me/profile")).data,

  updateProfile: async (patch: UpdateAccountProfileRequest) =>
    (await apiV2.patch<AccountProfile>("/me/profile", patch)).data,

  listSessions: async () => (await apiV2.get<AccountSessionListResponse>("/me/sessions")).data.sessions,

  revokeSession: async (sessionId: string) =>
    (await apiV2.post<AccountSessionRevokeResponse>(`/me/sessions/${encodeURIComponent(sessionId)}/revoke`)).data,

  /** Ends every session of the account, including this one. */
  revokeAllSessions: async () => (await apiV2.post<AccountSessionRevokeResponse>("/me/sessions/revoke-all")).data,

  /** Ends the current session too (ADR 0057): the caller signs in again afterwards. */
  changePassword: async (body: ChangePasswordRequest) =>
    (await api.post<MessageResponse>("/auth/change-password", body)).data,

  /**
   * ACC-026: `current_password` for password accounts; without it the API accepts only a
   * sign-in made within the re-authentication window, otherwise 403 REAUTHENTICATION_REQUIRED.
   */
  deactivate: async (currentPassword: string | null) => {
    const body: DeactivateAccountRequest = currentPassword ? { current_password: currentPassword } : {};
    return (await api.post<MessageResponse>("/users/me/deactivate", body)).data;
  },
};
