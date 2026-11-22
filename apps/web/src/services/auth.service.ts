import api, { publicApi } from "./api";
import { storageStore } from "@/store";
import { signOutMessaging } from "@/lib/messaging/signout";
import { allowCallingRegistration, signOutCalling } from "@/lib/calling/signout";
import { accountSession, clearBrowserSession } from "@/lib/auth/session";
import type { LoginResponse, MessageResponse } from "@/types/auth";
import type { OtpVerifyResponse } from "@/types/generated/meeting-service";
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

/** Thrown when the account needs its authenticator code to finish signing in. */
export class MfaRequired extends Error {
  constructor(readonly ticket: string) { super("MFA_REQUIRED"); }
}
function mfaOrThrow(e: unknown): never {
  const err = (e as { response?: { data?: { error?: { code?: string; mfaTicket?: string } } } }).response?.data?.error;
  if (err?.code === "MFA_REQUIRED" && err.mfaTicket) throw new MfaRequired(err.mfaTicket);
  throw e;
}

export const authService = {
  requestPhoneCode: async (phoneNumber: string) =>
    (await publicApi.post<{ challengeId: string }>("/auth/phone/request", { phoneNumber })).data,

  verifyPhoneCode: async (challengeId: string, code: string, rememberMe = true) => {
    const response = await publicApi.post<OtpVerifyResponse>("/auth/phone/verify", { challengeId, code }).catch(mfaOrThrow);
    await establish(response.data as unknown as LoginResponse, rememberMe);
    return response.data;
  },

  /**
   * Passwordless sign-in and sign-up (one flow). Step 1: email a 6-digit code and a one-click
   * link (valid 10 minutes). The answer is the same for every address; never infer from it.
   */
  requestCode: async (email: string) => {
    const response = await publicApi.post<MessageResponse>("/auth/otp/request", { email });
    return response.data;
  },

  /**
   * Step 2 (code): the first correct code creates the account (`is_new_account`). An account
   * with an authenticator app answers MFA_REQUIRED: the caller then asks for that code.
   */
  verifyCode: async (email: string, code: string, rememberMe = true) => {
    const response = await publicApi.post<OtpVerifyResponse>("/auth/otp/verify", { email, code }).catch(mfaOrThrow);
    await establish(response.data as unknown as LoginResponse, rememberMe);
    return response.data;
  },

  /** Step 3 (two-step accounts): the authenticator code, or a recovery code. */
  verifyMfa: async (mfaTicket: string, code: string, rememberMe = true) => {
    const response = await publicApi.post<OtpVerifyResponse>("/auth/mfa/verify", { mfaTicket, code });
    await establish(response.data as unknown as LoginResponse, rememberMe);
    return response.data;
  },

  /** Step 2 (email link): POSTed from /auth/magic after a click, never on page load. */
  verifyLink: async (email: string, token: string) => {
    const response = await publicApi.post<OtpVerifyResponse>("/auth/otp/verify-link", { email, token }).catch(mfaOrThrow);
    await establish(response.data as unknown as LoginResponse, true);
    return response.data;
  },

  /** Undo an email change from the link sent to the previous address. */
  revertEmailChange: async (token: string) => {
    const response = await publicApi.post<MessageResponse>("/auth/email-change/revert", { token });
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

};
