"use client";

import React, { useState } from "react";
import { useRouter } from "next/navigation";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import AuthHeader from "@/components/auth/AuthHeader";
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
    <div className="min-h-screen flex items-center justify-center bg-gray-50 px-4">
      <div className="relative z-10 bg-white rounded-2xl sm:rounded-[40px] shadow-2xl w-full max-w-[520px] px-5 py-6 sm:p-8 md:p-12 my-4 sm:my-6">
        {/* Header */}
        <AuthHeader
          title="Forgot Password?"
          subtitle="Enter your email to receive a password reset code."
        />

        {/* Error Message */}
        {error && (
          <div className="bg-red-50 border border-red-200 text-red-600 text-sm rounded-lg px-4 py-2 mb-4">
            {error}
          </div>
        )}

        {/* Form */}
        <form className="space-y-4" onSubmit={handleSubmit}>
          <AuthInput
            label="Email"
            type="email"
            placeholder="you@example.com"
            value={email}
            onChange={(e) => {
              setEmail(e.target.value);
              setError("");
            }}
            error={error}
          />

          <Button
            type="submit"
            size="lg"
            className="w-full"
            disabled={isLoading}
          >
            {isLoading ? "Sending..." : "Send Reset Code"}
          </Button>
        </form>

        {/* Back to Login */}
        <div className="text-center mt-4">
          <Button
            variant="link"
            onClick={() => router.push("/login")}
            className="text-sm"
          >
            ← Back to Login
          </Button>
        </div>
      </div>
    </div>
  );
};

export default ForgotPasswordPage;
