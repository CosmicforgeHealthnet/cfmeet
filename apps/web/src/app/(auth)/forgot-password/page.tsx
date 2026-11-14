"use client";

import React, { useState } from "react";
import { useRouter } from "next/navigation";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import Link from "next/link";
import { KeyRound } from "lucide-react";
import { AxiosError } from "axios";
import type { ApiErrorResponse } from "@/types/auth";
import { authService } from "@/services/auth.service";

const ForgotPasswordPage = () => {
  const router = useRouter();
  const [email, setEmail] = useState("");
  const [error, setError] = useState("");
  const [isLoading, setIsLoading] = useState(false);

  const validate = () => {
    if (!email.trim()) {
      setError("Email is required");
      return false;
    }
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
      setError("Invalid email address");
      return false;
    }
    return true;
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!validate()) return;

    setIsLoading(true);
    setError("");

    try {
      await authService.forgotPassword({ email: email.trim() });
      // Navigate to verify token page with email
      router.push(
        `/forgot-password/verify-token?email=${encodeURIComponent(email.trim())}`,
      );
    } catch (err) {
      const axiosError = err as AxiosError<ApiErrorResponse>;
      setError(
        axiosError.response?.data?.error?.message ||
          "Failed to send reset code. Please try again.",
      );
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div>
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <KeyRound className="size-6" aria-hidden />
      </span>
      <h1 className="mt-6 text-3xl font-bold tracking-tight text-foreground">Forgot your password?</h1>
      <p className="mt-2 text-base text-muted-foreground">
        Enter your account email and we&apos;ll send you a code to reset it.
      </p>

      <form className="mt-8 space-y-5" onSubmit={handleSubmit} noValidate>
        <AuthInput
          label="Email"
          type="email"
          autoComplete="email"
          placeholder="you@example.com"
          value={email}
          onChange={(e) => {
            setEmail(e.target.value);
            setError("");
          }}
          error={error}
        />
        <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" disabled={isLoading}>
          {isLoading ? "Sending…" : "Send reset code"}
        </Button>
      </form>

      <p className="mt-8 border-t border-border pt-6 text-center text-sm text-muted-foreground">
        Remembered it?{" "}
        <Link href="/login" className="font-semibold text-primary hover:underline">Back to sign in</Link>
      </p>
    </div>
  );
};

export default ForgotPasswordPage;
