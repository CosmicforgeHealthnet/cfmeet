import { send } from "@/lib/api/client";
import { ROUTES } from "@/lib/api/routes";
import type {
  ChangePasswordRequest,
  ForgotPasswordRequest,
  GoogleExchangeRequest,
  LoginRequest,
  LoginResponse,
  MessageResponse,
  RegisterRequest,
  RegisterResponse,
  ResendVerificationRequest,
  ResetPasswordRequest,
  VerifyEmailRequest,
  VerifyResetTokenRequest,
  VerifyResetTokenResponse,
} from "@/lib/api/generated/meeting-service";

/** Account operations (served under /api/v1; v1 is the account service's current version). */
export const authApi = {
  login: (data: LoginRequest) => send<LoginResponse>(ROUTES.login, [], { data, anonymous: true }),
  register: (data: RegisterRequest) => send<RegisterResponse>(ROUTES.register, [], { data, anonymous: true }),
  verifyEmail: (data: VerifyEmailRequest) =>
    send<MessageResponse>(ROUTES.verifyEmail, [], { data, anonymous: true }),
  resendVerification: (data: ResendVerificationRequest) =>
    send<MessageResponse>(ROUTES.resendVerification, [], { data, anonymous: true }),
  forgotPassword: (data: ForgotPasswordRequest) =>
    send<MessageResponse>(ROUTES.forgotPassword, [], { data, anonymous: true }),
  verifyResetToken: (data: VerifyResetTokenRequest) =>
    send<VerifyResetTokenResponse>(ROUTES.verifyResetToken, [], { data, anonymous: true }),
  resetPassword: (data: ResetPasswordRequest) =>
    send<MessageResponse>(ROUTES.resetPassword, [], { data, anonymous: true }),
  /** Completes Google sign-in with the one-time code (exchange mode). */
  googleExchange: (data: GoogleExchangeRequest) =>
    send<LoginResponse>(ROUTES.googleExchange, [], { data, anonymous: true }),
  logout: () => send<MessageResponse>(ROUTES.logout, []),
  changePassword: (data: ChangePasswordRequest) => send<MessageResponse>(ROUTES.changePassword, [], { data }),
};
