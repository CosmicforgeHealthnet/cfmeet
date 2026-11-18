/**
 * Password-account request / response shapes that the passwordless Meeting contract (PR #97) no
 * longer publishes. Copied verbatim from the last generated contract so the existing mobile
 * sign-in screens keep compiling and behaving as before, until they move to the one-time-code
 * flow. Do not add new uses.
 */
export type ChangePasswordRequest = {
  confirm_password: string;
  current_password: string;
  new_password: string;
};

export type ForgotPasswordRequest = {
  email: string;
};

export type LoginRequest = {
  email: string;
  password: string;
};

export type RegisterRequest = {
  confirm_password: string;
  email: string;
  password: string;
  username: string;
};

export type ResetPasswordRequest = {
  confirm_password: string;
  email: string;
  new_password: string;
  token: string;
};

export type VerifyEmailRequest = {
  email: string;
  token: string;
};

export type VerifyResetTokenRequest = {
  email: string;
  token: string;
};

export type RegisterResponse = {
  email: string;
  role: string;
  status: string;
  user_id: string;
};

export type ResendVerificationRequest = {
  email: string;
};

export type VerifyResetTokenResponse = {
  message: string;
  valid: boolean;
};
