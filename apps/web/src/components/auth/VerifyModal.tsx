"use client";

import React, { useEffect, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import {
  InputOTP,
  InputOTPGroup,
  InputOTPSlot,
} from "@/components/ui/input-otp";
import { Mail } from "lucide-react";
import { useVerifyEmail } from "@/hooks";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { authService } from "@/services/auth.service";

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
  // Resend cooldown so the code can't be requested in a burst.
  const [cooldown, setCooldown] = useState(30);

  useEffect(() => {
    if (cooldown <= 0) return;
    const t = setTimeout(() => setCooldown((c) => c - 1), 1000);
    return () => clearTimeout(t);
  }, [cooldown]);

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
          void authService.forgetLocalSession();
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
      setResendMessage("A new code is on its way to your inbox.");
      setCooldown(30);
    } catch (err) {
      const axiosError = err as AxiosError<ApiErrorResponse>;
      setError(
        axiosError.response?.data?.error?.message ||
          "Failed to resend code. Please try again.",
      );
    }
  };

  const loading = externalLoading !== undefined ? externalLoading : verifyEmail.isPending;

  return (
    <div>
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <Mail className="size-6" aria-hidden />
      </span>
      <h1 className="mt-6 text-3xl font-bold tracking-tight text-foreground">{title === "Verify your Email" ? "Check your email" : title}</h1>
      <p className="mt-2 text-base text-muted-foreground">
        We sent a 6-digit code to{" "}
        {displayEmail ? <span className="font-semibold text-foreground">{displayEmail}</span> : "your email"}. Enter it below.
      </p>

      <form className="mt-8 space-y-5" onSubmit={handleSubmit} noValidate>
        <InputOTP
          maxLength={6}
          value={otp}
          onChange={(value) => {
            setOtp(value);
            setError("");
          }}
          containerClassName="w-full"
          aria-label="6-digit code"
          autoComplete="one-time-code"
        >
          <InputOTPGroup className="flex w-full justify-between gap-2">
            {[0, 1, 2, 3, 4, 5].map((index) => (
              <InputOTPSlot
                key={index}
                index={index}
                className="h-14 flex-1 rounded-xl border border-border bg-background text-xl font-semibold text-foreground first:rounded-xl last:rounded-xl"
              />
            ))}
          </InputOTPGroup>
        </InputOTP>

        {(error || customError) && (
          <div role="alert" className="rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
            {customError || error}
          </div>
        )}
        {resendMessage && (
          <div role="status" className="rounded-xl border border-[var(--success)]/30 bg-[var(--success)]/10 px-4 py-3 text-sm text-foreground">
            {resendMessage}
          </div>
        )}

        <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" loading={loading}>
          Verify
        </Button>
      </form>

      <p className="mt-6 text-center text-sm text-muted-foreground">
        Didn&apos;t get the code?{" "}
        {onResend ? (
          cooldown > 0 ? (
            <span className="tabular-nums">Resend in {cooldown}s</span>
          ) : (
            <button type="button" onClick={handleResend} className="font-semibold text-primary hover:underline">
              Resend code
            </button>
          )
        ) : (
          "Check your spam folder."
        )}
      </p>

      <p className="mt-8 border-t border-border pt-6 text-center text-sm text-muted-foreground">
        {showReturnToLogin ? (
          <Link href="/login" className="font-semibold text-primary hover:underline">Back to sign in</Link>
        ) : (
          <>
            Wrong email?{" "}
            <Link href="/signup" className="font-semibold text-primary hover:underline">Start again</Link>
          </>
        )}
      </p>
    </div>
  );
};

export default VerifyModal;
