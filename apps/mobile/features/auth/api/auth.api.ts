import { api, publicApi } from "@/lib/api/client";
import { API_URL } from "@/lib/constants";
import type {
  LoginRequest,
  LoginResponse,
  RegisterRequest,
  RegisterResponse,
  VerifyEmailRequest,
  VerifyEmailResponse,
  ResendVerificationRequest,
  ResendVerificationResponse,
  ForgotPasswordRequest,
  ForgotPasswordResponse,
  VerifyResetTokenRequest,
  VerifyResetTokenResponse,
  ResetPasswordRequest,
  ResetPasswordResponse,
  ChangePasswordRequest,
  ChangePasswordResponse,
} from "../types";

export const authApi = {
  async login(data: LoginRequest): Promise<LoginResponse> {
    console.log("=== API URL ===", API_URL);
    console.log("=== FULL LOGIN URL ===", `${API_URL}/auth/login`);
    const response = await publicApi.post<LoginResponse>("/auth/login", data);
    return response.data;
  },

  async register(data: RegisterRequest): Promise<RegisterResponse> {
    const response = await publicApi.post<RegisterResponse>("/auth/register", data);
    return response.data;
  },

  async verifyEmail(data: VerifyEmailRequest): Promise<VerifyEmailResponse> {
    const response = await publicApi.post<VerifyEmailResponse>("/auth/verify-email", data);
    return response.data;
  },

  async resendVerification(data: ResendVerificationRequest): Promise<ResendVerificationResponse> {
    const response = await publicApi.post<ResendVerificationResponse>(
      "/auth/resend-verification",
      data
    );
    return response.data;
  },

  async forgotPassword(data: ForgotPasswordRequest): Promise<ForgotPasswordResponse> {
    const response = await publicApi.post<ForgotPasswordResponse>(
      "/auth/forgot-password",
      data
    );
    return response.data;
  },

  async verifyResetToken(data: VerifyResetTokenRequest): Promise<VerifyResetTokenResponse> {
    const response = await publicApi.post<VerifyResetTokenResponse>(
      "/auth/verify-reset-token",
      data
    );
    return response.data;
  },

  async resetPassword(data: ResetPasswordRequest): Promise<ResetPasswordResponse> {
    const response = await publicApi.post<ResetPasswordResponse>(
      "/auth/reset-password",
      data
    );
    return response.data;
  },

  async logout(): Promise<void> {
    await api.post("/auth/logout");
  },

  async changePassword(data: ChangePasswordRequest): Promise<ChangePasswordResponse> {
    const response = await api.post<ChangePasswordResponse>("/auth/change-password", data);
    return response.data;
  },
};
