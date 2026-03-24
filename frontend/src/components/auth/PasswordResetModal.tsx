"use client";

import React, { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import AuthHeader from "./AuthHeader";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import { authService } from "@/services/auth.service";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";

interface PasswordResetModalProps {
  email: string;
  token: string;
}

const PasswordResetModal = ({ email, token }: PasswordResetModalProps) => {
  const router = useRouter();
  const [form, setForm] = useState({
    password: "",
    confirmPassword: "",
  });
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [isLoading, setIsLoading] = useState(false);

  const handleChange =
    (field: string) => (e: React.ChangeEvent<HTMLInputElement>) => {
      setForm((prev) => ({ ...prev, [field]: e.target.value }));
      setErrors((prev) => ({ ...prev, [field]: "", general: "" }));
    };

  const validate = () => {
    const newErrors: Record<string, string> = {};

    if (!form.password) {
      newErrors.password = "Password is required";
    } else if (form.password.length < 8) {
      newErrors.password = "Password must be at least 8 characters";
    }

    if (form.password !== form.confirmPassword) {
      newErrors.confirmPassword = "Passwords do not match";
    }

    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!validate()) return;

    setIsLoading(true);
    setErrors({});

    try {
      await authService.resetPassword({
        email,
        token,
        new_password: form.password,
        confirm_password: form.confirmPassword,
      });

      // Success - redirect to login
      router.push("/login?reset=success");
    } catch (err) {
      const axiosError = err as AxiosError<ApiErrorResponse>;
      setErrors({
        general:
          axiosError.response?.data?.error?.message ||
          "Failed to reset password. Please try again.",
      });
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="relative z-10 bg-white rounded-[40px] shadow-2xl w-full p-6 md:p-9">
      {/* Header */}
      <AuthHeader title="Password Reset!" subtitle="Create New Password." />

      {/* General Error */}
      {errors.general && (
        <div className="bg-red-50 border border-red-200 text-red-600 text-sm rounded-lg px-4 py-2 mb-4">
          {errors.general}
        </div>
      )}

      {/* Form */}
      <form className="space-y-2" onSubmit={handleSubmit}>
        <AuthInput
          label="Password"
          type="password"
          placeholder="Create a Password"
          value={form.password}
          onChange={handleChange("password")}
          error={errors.password}
        />

        <AuthInput
          label="Confirm Password"
          type="password"
          placeholder="Confirm your password"
          value={form.confirmPassword}
          onChange={handleChange("confirmPassword")}
          error={errors.confirmPassword}
        />

        {/* Submit Button */}
        <Button size="lg" className="w-full mt-4" disabled={isLoading}>
          {isLoading ? "Resetting..." : "Submit"}
        </Button>
      </form>

      {/* Back to Login */}
      <div className="text-center mt-6">
        <Link href="/login">
          <Button variant="link">Return to Log In Page</Button>{" "}
        </Link>
      </div>
    </div>
  );
};

export default PasswordResetModal;
