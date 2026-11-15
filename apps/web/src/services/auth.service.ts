import api, { publicApi } from "./api";
import { storageStore } from "@/store";
import { signOutMessaging } from "@/lib/messaging/signout";
import { allowCallingRegistration, signOutCalling } from "@/lib/calling/signout";
import { accountSession, clearBrowserSession } from "@/lib/auth/session";
import type {
  RegisterRequest,
  RegisterResponse,
  VerifyEmailRequest,
  ResendVerificationRequest,
  LoginRequest,
  LoginResponse,
  MessageResponse,
  VerifyResetTokenRequest,
  VerifyResetTokenResponse,
} from "@/types/auth";
import apiV2 from "./v2/api";
import type { AccountSessionRevokeResponse, GoogleExchangeRequest } from "@/types/generated/meeting-service";
import { DEFAULT_WORKSPACE_KEY, TAB_WORKSPACE_KEY } from "@/lib/workspace/workspace-core";

/**
 * Stores a fresh sign-in: refresh token in the httpOnly session cookie, access token in
 * memory, non-secret user summary for the UI (ACC-019).
 */
async function establish(response: LoginResponse, remember: boolean) {
  await accountSession.establish(response.access_token, response.refresh_token, remember);
  storageStore.setUser(response.user);
  // A fresh sign-in may register this browser's Calling endpoint again (S-10).
  allowCallingRegistration();
  await declareThisBrowser(response.access_token);
}

/** How long sign-in waits for the device declaration before continuing without it. */
const DECLARE_WAIT_MS = 5_000;

/**
 * Multi-device policy (ADR 0060): bind this fresh sign-in to this browser's account device
 * right away, inside the declaration window, so private messaging can start on its own
 * (the first browser of an account becomes its trusted device; others are linked). The
 * device key is generated once per browser profile and reused. Never blocks sign-in: on
 * any failure or timeout the sign-in simply stays unclassified (Meetings, Calling and
 * Organizations are unaffected) and Messaging retries.
 */
async function declareThisBrowser(accessToken: string): Promise<void> {
  if (typeof window === "undefined") return;
  try {
    const { accountDevice } = await import("@/lib/auth/account-device");
    const { browserDeviceLabel } = await import("@/lib/auth/device-label");
    await Promise.race([
      accountDevice.declare(accessToken, browserDeviceLabel(navigator.userAgent)).catch(() => null),
      new Promise((resolve) => setTimeout(resolve, DECLARE_WAIT_MS)),
    ]);
  } catch {
    /* best effort */
  }
}

export const authService = {
  register: async (data: RegisterRequest) => {
    const response = await publicApi.post<RegisterResponse>("/auth/register", data);
    return response.data;
  },

  verifyEmail: async (data: VerifyEmailRequest) => {
    const response = await publicApi.post<MessageResponse>("/auth/verify-email", data);
    return response.data;
  },

  resendVerification: async (data: ResendVerificationRequest) => {
    const response = await publicApi.post<MessageResponse>("/auth/resend-verification", data);
    return response.data;
  },

  /**
   * Password sign-in. The browser calls the account authority directly (so per-address
   * limits and the session's client label see the real client); only the resulting refresh
   * token is handed to the app's session route.
   */
  login: async (data: LoginRequest, rememberMe = false) => {
    const response = await publicApi.post<LoginResponse>("/auth/login", data);
    await establish(response.data, rememberMe);
    return response.data;
  },

  /** ACC-005: one-time Google exchange code → tokens (never tokens in the URL). */
  exchangeGoogleCode: async (exchangeCode: string) => {
    const body: GoogleExchangeRequest = { exchange_code: exchangeCode };
    const response = await publicApi.post<LoginResponse>("/auth/oauth/google/exchange", body);
    await establish(response.data, true);
    return response.data;
  },

  /** Legacy Google callback (tokens in the query) while GOOGLE_OAUTH_TOKEN_DELIVERY=legacy_query. */
  adoptLegacyGoogleTokens: async (response: LoginResponse) => {
    await establish(response, true);
    return response;
  },

  /** Ends this session at the API (other sessions stay signed in) and in this browser. */
  logout: async () => {
    try {
      // While still signed in: revoke this browser's Calling endpoint (S-10) and messaging
      // device, erase its keys. Both are bounded and never throw.
      await Promise.all([signOutCalling(), signOutMessaging()]);
      await api.post<MessageResponse>("/auth/logout");
    } finally {
      await authService.forgetLocalSession();
    }
  },

  /**
   * "Sign out everywhere": ends every session of the account at the API
   * (`POST /api/v2/me/sessions/revoke-all`), then this browser's state.
   */
  logoutEverywhere: async () => {
    await Promise.all([signOutCalling(), signOutMessaging()]);
    // A failure keeps this browser signed in (nothing was revoked); the caller shows it.
    const result = (await apiV2.post<AccountSessionRevokeResponse>("/me/sessions/revoke-all")).data;
    await authService.forgetLocalSession();
    return result;
  },

  /**
   * Standard re-authentication: ends this (too old) sign-in at the API and in this browser,
   * so the login page starts a fresh one. Local encrypted data is not touched.
   */
  endSessionForReauthentication: async () => {
    try {
      await api.post<MessageResponse>("/auth/logout");
    } catch {
      /* the session may already be gone; the browser side is cleared regardless */
    } finally {
      await authService.forgetLocalSession();
    }
  },

  /** Browser-side sign-out only (after the API already ended the session). */
  forgetLocalSession: async () => {
    await clearBrowserSession();
    storageStore.clearUser();
    if (typeof window !== "undefined") {
      try {
        window.sessionStorage.removeItem(TAB_WORKSPACE_KEY);
        window.localStorage.removeItem(DEFAULT_WORKSPACE_KEY);
      } catch {
        /* storage disabled */
      }
    }
  },

  initiateGoogleOAuth: () => {
    const apiUrl = process.env.NEXT_PUBLIC_API_URL || "http://localhost:3001/api/v1";
    window.location.href = `${apiUrl}/auth/oauth/google`;
  },

  verifyResetToken: async (data: VerifyResetTokenRequest) => {
    const response = await publicApi.post<VerifyResetTokenResponse>("/auth/verify-reset-token", data);
    return response.data;
  },

  forgotPassword: async (data: { email: string }) => {
    const response = await publicApi.post<MessageResponse>("/auth/forgot-password", data);
    return response.data;
  },

  resetPassword: async (data: {
    email: string;
    token: string;
    new_password: string;
    confirm_password: string;
  }) => {
    const response = await publicApi.post<MessageResponse>("/auth/reset-password", data);
    return response.data;
  },
};
