import api from "./api";
import { cookieStore } from "@/store";
import { storageStore } from "@/store";
import type {
  RegisterRequest,
  RegisterResponse,
  VerifyEmailRequest,
  ResendVerificationRequest,
  LoginRequest,
  LoginResponse,
  RefreshTokenRequest,
  RefreshTokenResponse,
  MessageResponse,
  VerifyResetTokenRequest,
  VerifyResetTokenResponse,
} from "@/types/auth";

export const authService = {
  register: async (data: RegisterRequest) => {
    const response = await api.post<RegisterResponse>("/auth/register", data);
    return response.data;
  },

  verifyEmail: async (data: VerifyEmailRequest) => {
    const response = await api.post<MessageResponse>(
      "/auth/verify-email",
      data,
    );
    return response.data;
  },

  resendVerification: async (data: ResendVerificationRequest) => {
    const response = await api.post<MessageResponse>(
      "/auth/resend-verification",
      data,
    );
    return response.data;
  },

  login: async (data: LoginRequest, rememberMe = false) => {
    const response = await api.post<LoginResponse>("/auth/login", data);
    const { access_token, refresh_token, user } = response.data;
    cookieStore.setTokens(access_token, refresh_token, rememberMe);
    storageStore.setUser(user);
    return response.data;
  },

  refresh: async () => {
    const refreshToken = cookieStore.getRefreshToken();
    if (!refreshToken) throw new Error("No refresh token");

    const response = await api.post<RefreshTokenResponse>("/auth/refresh", {
      refresh_token: refreshToken,
    } as RefreshTokenRequest);
    const { access_token, refresh_token } = response.data;
    cookieStore.setTokens(access_token, refresh_token);
    return response.data;
  },

  logout: async () => {
    try {
      await api.post<MessageResponse>("/auth/logout");
    } finally {
      cookieStore.clearTokens();
      storageStore.clearUser();
    }
  },

  initiateGoogleOAuth: () => {
    const apiUrl =
      process.env.NEXT_PUBLIC_API_URL || "http://localhost:3001/api/v1";
    window.location.href = `${apiUrl}/auth/oauth/google`;
  },

  verifyResetToken: async (data: VerifyResetTokenRequest) => {
    const response = await api.post<VerifyResetTokenResponse>(
      "/auth/verify-reset-token",
      data,
    );
    return response.data;
  },

  forgotPassword: async (data: { email: string }) => {
    const response = await api.post<MessageResponse>(
      "/auth/forgot-password",
      data,
    );
    return response.data;
  },

  resetPassword: async (data: {
    email: string;
    token: string;
    new_password: string;
    confirm_password: string;
  }) => {
    const response = await api.post<MessageResponse>(
      "/auth/reset-password",
      data,
    );
    return response.data;
  },
};
