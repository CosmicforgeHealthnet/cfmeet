"use client";

import React, { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { LockKeyhole } from "lucide-react";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import { authService } from "@/services/auth.service";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { formErrorsOf } from "@/lib/validation-errors";

/** API field → form input; other problems (email, token) show in the alert. */
const RESET_FIELDS: Record<string, string> = {
  new_password: "password",
  confirm_password: "confirmPassword",
};

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
      // Structured field problems (error.details) land on the matching inputs.
      const fieldErrors = formErrorsOf(err, RESET_FIELDS);
      if (Object.keys(fieldErrors).length > 0) {
        setErrors(fieldErrors);
        return;
      }
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
    <div>
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <LockKeyhole className="size-6" aria-hidden />
      </span>
      <h1 className="mt-6 text-3xl font-bold tracking-tight text-foreground">Set a new password</h1>
      <p className="mt-2 text-base text-muted-foreground">Choose a password you haven&apos;t used here before.</p>

      {errors.general && (
        <div role="alert" className="mt-6 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {errors.general}
        </div>
      )}

      <form className="mt-8 space-y-5" onSubmit={handleSubmit} noValidate>
        <AuthInput
          label="New password"
          type="password"
          autoComplete="new-password"
          placeholder="At least 8 characters"
          value={form.password}
          onChange={handleChange("password")}
          error={errors.password}
        />
        <AuthInput
          label="Confirm new password"
          type="password"
          autoComplete="new-password"
          placeholder="Re-enter your password"
          value={form.confirmPassword}
          onChange={handleChange("confirmPassword")}
          error={errors.confirmPassword}
        />
        <Button className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" disabled={isLoading}>
          {isLoading ? "Saving…" : "Reset password"}
        </Button>
      </form>

      <p className="mt-8 border-t border-border pt-6 text-center text-sm text-muted-foreground">
        <Link href="/login" className="font-semibold text-primary hover:underline">Back to sign in</Link>
      </p>
    </div>
  );
};

export default PasswordResetModal;
