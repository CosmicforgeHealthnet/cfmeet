import type {
  ChangePasswordRequest as ContractChangePasswordRequest,
  ForgotPasswordRequest as ContractForgotPasswordRequest,
  LoginRequest as ContractLoginRequest,
  RegisterRequest as ContractRegisterRequest,
  ResetPasswordRequest as ContractResetPasswordRequest,
  UserInfo as ContractUserInfo,
  VerifyEmailRequest as ContractVerifyEmailRequest,
  VerifyResetTokenRequest as ContractVerifyResetTokenRequest,
} from "@/lib/api/generated/meeting-service";

/** Request bodies are the contract's (generated) DTOs. */
export type ChangePasswordRequest = ContractChangePasswordRequest;
export type ForgotPasswordRequest = ContractForgotPasswordRequest;
export type RegisterRequest = ContractRegisterRequest;
export type ResetPasswordRequest = ContractResetPasswordRequest;
export type VerifyEmailRequest = ContractVerifyEmailRequest;
export type VerifyResetTokenRequest = ContractVerifyResetTokenRequest;

/** The form's sign-in input: the contract body plus the local "remember me" choice. */
export type LoginRequest = ContractLoginRequest & { remember_me?: boolean };

/**
 * The signed-in user kept on the device: the login `user` (id, username, role, status) plus the
 * email the user signed in with (the login body carries no email).
 */
export type UserInfo = ContractUserInfo & { email?: string };

export interface AuthState {
  user: UserInfo | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  setUser: (user: UserInfo | null) => void;
  setIsAuthenticated: (value: boolean) => void;
  setIsLoading: (value: boolean) => void;
  logout: () => void;
}
