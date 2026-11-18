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
  MessageResponse,
  UpdateAccountProfileRequest,
} from "@/types/generated/meeting-service";

/**
 * The password-change body. The passwordless backend (PR #97) no longer publishes this
 * operation in the served contract; the type is kept here until the web sign-in screens move
 * to the one-time-code flow, so the existing screens keep compiling unchanged.
 */
export type StepUpPurpose = "DEACTIVATE_ACCOUNT" | "CHANGE_EMAIL" | "DELETE_ACCOUNT" | "DISABLE_MFA";

export const accountService = {
  getProfile: async () => (await apiV2.get<AccountProfile>("/me/profile")).data,

  updateProfile: async (patch: UpdateAccountProfileRequest) =>
    (await apiV2.patch<AccountProfile>("/me/profile", patch)).data,

  listSessions: async () => (await apiV2.get<AccountSessionListResponse>("/me/sessions")).data.sessions,

  revokeSession: async (sessionId: string) =>
    (await apiV2.post<AccountSessionRevokeResponse>(`/me/sessions/${encodeURIComponent(sessionId)}/revoke`)).data,

  /** Ends every session of the account, including this one. */
  revokeAllSessions: async () => (await apiV2.post<AccountSessionRevokeResponse>("/me/sessions/revoke-all")).data,

  /**
   * Step-up for sensitive actions: a code goes to the account's current address (10 minutes);
   * verifying it returns a single-use token (5 minutes) bound to this session and purpose.
   */
  requestStepUp: async (purpose: StepUpPurpose) =>
    (await apiV2.post<{ message: string; expiresInSecs: number }>("/me/step-up/request", { purpose })).data,
  verifyStepUp: async (purpose: StepUpPurpose, code: string) =>
    (await apiV2.post<{ stepUpToken: string; expiresInSecs: number }>("/me/step-up/verify", { purpose, code })).data,

  /** Upload a profile photo (PNG, JPEG or WebP, at most 512 KiB); returns the updated profile. */
  uploadAvatar: async (file: File) =>
    (await apiV2.put<AccountProfile>("/me/avatar", file, { headers: { "Content-Type": file.type } })).data,

  /** Sign out every other device (this one stays signed in). */
  revokeOtherSessions: async () => (await apiV2.post<AccountSessionRevokeResponse>("/me/sessions/revoke-others")).data,

  /** Two-step sign-in with an authenticator app. */
  mfaStatus: async () => (await apiV2.get<{ totpEnabled: boolean; recoveryCodesRemaining: number }>("/me/mfa")).data,
  mfaSetup: async () => (await apiV2.post<{ otpauthUri: string; secret: string }>("/me/mfa/totp/setup", {})).data,
  mfaConfirm: async (code: string) => (await apiV2.post<{ recoveryCodes: string[] }>("/me/mfa/totp/confirm", { code })).data,
  mfaDisable: async (stepUpToken: string) => (await apiV2.post("/me/mfa/disable", { stepUpToken })).data,

  /** Everything CF Meet keeps about you, as one JSON document. */
  dataExport: async () => (await apiV2.get<unknown>("/me/data-export")).data,
  /** Delete the account for good, with a DELETE_ACCOUNT step-up token. */
  deleteAccount: async (stepUpToken: string) => (await apiV2.post("/me/account/delete", { stepUpToken })).data,

  /** Deactivate the account with a DEACTIVATE_ACCOUNT step-up token. */
  deactivate: async (stepUpToken: string) =>
    (await api.post<MessageResponse>("/users/me/deactivate", { step_up_token: stepUpToken })).data,

  /** Change email: a code goes to the new address; verifying it ends every other session. */
  requestEmailChange: async (newEmail: string, stepUpToken: string) =>
    (await apiV2.post<{ message: string; expiresInSecs: number }>("/me/email/change/request", { newEmail, stepUpToken })).data,
  verifyEmailChange: async (newEmail: string, code: string) =>
    (await apiV2.post<{ email: string; otherSessionsEnded: boolean }>("/me/email/change/verify", { newEmail, code })).data,
};
