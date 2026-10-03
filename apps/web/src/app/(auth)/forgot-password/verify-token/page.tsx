"use client";

import { useSearchParams, useRouter } from "next/navigation";
import { Suspense, useState } from "react";
import VerifyModal from "@/components/auth/VerifyModal";
import { useVerifyResetToken } from "@/hooks";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { authService } from "@/services/auth.service";

function VerifyTokenContent() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const email = searchParams.get("email") || "";
  const [error, setError] = useState("");

  const verifyToken = useVerifyResetToken();

  const handleVerify = (token: string) => {
    if (!email) {
      setError("Email is missing. Please start over.");
      return;
    }

    verifyToken.mutate(
      { email, token },
      {
        onSuccess: () => {
          // Navigate to reset password page with email and token
          router.push(
            `/forgot-password/reset?email=${encodeURIComponent(email)}&token=${encodeURIComponent(token)}`,
          );
        },
        onError: (err) => {
          const axiosError = err as AxiosError<ApiErrorResponse>;
          setError(
            axiosError.response?.data?.error?.message ||
              "Invalid or expired token. Please try again.",
          );
        },
      },
    );
  };

  const handleResend = async () => {
    if (!email) {
      throw new Error("Email is missing");
    }
    await authService.forgotPassword({ email });
  };

  return (
    <VerifyModal
      email={email}
      title="Reset Password"
      maskedEmail
      showReturnToLogin={true}
      onVerify={handleVerify}
      onResend={handleResend}
      customError={error}
      isLoading={verifyToken.isPending}
    />
  );
}

export default function VerifyTokenPage() {
  return (
    <Suspense
      fallback={
        <div className="min-h-screen flex items-center justify-center bg-gray-50">
          <div className="text-gray-500">Loading...</div>
        </div>
      }
    >
      <VerifyTokenContent />
    </Suspense>
  );
}
