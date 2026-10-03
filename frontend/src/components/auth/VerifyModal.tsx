"use client";

import React, { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import {
  InputOTP,
  InputOTPGroup,
  InputOTPSlot,
} from "@/components/ui/input-otp";
import AuthHeader from "./AuthHeader";
import { useVerifyEmail } from "@/hooks";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { cookieStore, storageStore } from "@/store";

function maskEmail(email: string) {
  const [local, domain] = email.split("@");
  const visible = local.slice(0, 2);
  return `${visible}${"*".repeat(Math.max(local.length - 2, 0))}@${domain}`;
}

const VerifyModal = ({
  email = "",
  title = "Verify your Email",
  maskedEmail = false,
  showReturnToLogin = false,
  onVerify,
  onResend,
  customError,
  isLoading: externalLoading,
}: {
  email?: string;
  title?: string;
  maskedEmail?: boolean;
  showReturnToLogin?: boolean;
  onVerify?: (token: string) => void;
  onResend?: () => Promise<void>;
  customError?: string;
  isLoading?: boolean;
}) => {
  const router = useRouter();
  const verifyEmail = useVerifyEmail();

  const [otp, setOtp] = useState("");
  const [error, setError] = useState("");
  const [resendMessage, setResendMessage] = useState("");

  const displayEmail = maskedEmail ? maskEmail(email) : email;

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    setError("");

    if (otp.length !== 6) {
      setError("Please enter the 6-digit code");
      return;
    }

    // If custom onVerify is provided, use it (for password reset)
    if (onVerify) {
      onVerify(otp);
      return;
    }

    // Otherwise use default email verification
    if (!email) {
      setError("Email is missing. Please go back and try again.");
      return;
    }

    verifyEmail.mutate(
      { email, token: otp },
      {
        onSuccess: () => {
          // Clear any stored tokens so user can login fresh after verification
          cookieStore.clearTokens();
          storageStore.clearUser();
          router.push("/login");
        },
        onError: (err) => {
          const axiosError = err as AxiosError<ApiErrorResponse>;
          setError(
            axiosError.response?.data?.error?.message ||
              "Verification failed. Please try again.",
          );
        },
      },
    );
  };

  const handleResend = async () => {
    if (!email || !onResend) return;
    setResendMessage("");
    setError("");

    try {
      await onResend();
      setResendMessage("A new code has been sent to your email");
    } catch (err) {
      const axiosError = err as AxiosError<ApiErrorResponse>;
      setError(
        axiosError.response?.data?.error?.message ||
          "Failed to resend code. Please try again.",
      );
    }
  };

  return (
    <div className="relative z-10 flex flex-col items-center w-full max-w-[520px]">
      <div className="bg-white rounded-2xl sm:rounded-[40px] shadow-2xl w-full max-w-[520px] px-5 py-6 sm:p-8 md:p-12 my-4 sm:my-6 flex flex-col items-center">
        {/* Header */}
        <AuthHeader title={title} subtitle="Enter OTP code" />

        <div className="text-center mb-2 space-y-1">
          <p className="text-sm text-gray-400">
            We sent you a 6-digit code via your email
          </p>
          <p className="text-sm font-bold text-gray-900">{displayEmail}</p>
        </div>

        {/* OTP Input Group */}
        <form
          className="w-full flex flex-col items-start space-y-2"
          onSubmit={handleSubmit}
        >
          <InputOTP
            maxLength={6}
            value={otp}
            onChange={(value) => {
              setOtp(value);
              setError("");
            }}
            containerClassName="group flex items-center justify-between w-full"
          >
            <InputOTPGroup className="flex justify-between w-full">
              {[0, 1, 2, 3, 4, 5].map((index) => (
                <InputOTPSlot
                  key={index}
                  index={index}
                  className="w-9 h-8 sm:w-11 sm:h-9 md:w-13 md:h-10 rounded-sm border-none bg-sky-100 text-black text-lg sm:text-xl"
                />
              ))}
            </InputOTPGroup>
          </InputOTP>

          {/* Error - below OTP input */}
          {(error || customError) && (
            <div className="w-full bg-red-50 border border-red-200 text-red-600 text-sm rounded-lg px-4 py-2">
              {customError || error}
            </div>
          )}

          {/* Success message */}
          {resendMessage && (
            <div className="w-full bg-green-50 border border-green-200 text-green-600 text-sm rounded-lg px-4 py-2">
              {resendMessage}
            </div>
          )}

          <div>
            <p className="text-sm text-gray-500">
              Didn&apos;t receive code?{" "}
              <Button
                type="button"
                className="text-[#029CD4] font-bold p-1"
                variant="link"
                onClick={handleResend}
                disabled={!onResend}
              >
                Resend!
              </Button>
            </p>
          </div>

          {/* Submit Button */}
          <Button
            type="submit"
            size="lg"
            className="w-full"
            loading={
              externalLoading !== undefined
                ? externalLoading
                : verifyEmail.isPending
            }
          >
            Submit Code
          </Button>
        </form>

        {showReturnToLogin && (
          <Link href="/login" className="mt-2 text-sm ">
            <Button variant="link"> Return to Log In Page</Button>{" "}
          </Link>
        )}
      </div>
    </div>
  );
};

export default VerifyModal;
