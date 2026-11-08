"use client";

import React, { useState, useEffect, useSyncExternalStore } from "react";
import Link from "next/link";
import Image from "next/image";
import { useRouter } from "next/navigation";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import AuthHeader from "./AuthHeader";
import { Checkbox } from "../ui/checkbox";
import Divider from "./Divider";
import { useLogin } from "@/hooks";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { authService } from "@/services/auth.service";
import { accountSession } from "@/lib/auth/session";
import { normalizeAccountStatus, signInNotice } from "@/lib/account/account-core";
import { safeReturnPath } from "@/lib/organizations/organization-core";

/** `?next=` — only allow-listed same-origin paths (open-redirect safe). */
const noSubscription = () => () => {};

const returnPath = () =>
  (typeof window !== "undefined" && safeReturnPath(new URLSearchParams(window.location.search).get("next"))) || "/dashboard";

const LoginModal = () => {
  const router = useRouter();
  const login = useLogin();

  useEffect(() => {
    if (accountSession.hasSession()) {
      router.push(returnPath());
    }
  }, [router]);

  const [form, setForm] = useState({
    email: "",
    password: "",
  });
  const [rememberMe, setRememberMe] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // Why the user is here (password changed, signed out everywhere, session ended).
  const search = useSyncExternalStore(noSubscription, () => window.location.search, () => "");
  const notice = signInNotice(new URLSearchParams(search).get("reason"));

  const handleChange =
    (field: string) => (e: React.ChangeEvent<HTMLInputElement>) => {
      setForm((prev) => ({ ...prev, [field]: e.target.value }));
      setErrors((prev) => ({ ...prev, [field]: "", general: "" }));
    };

  const validate = () => {
    const newErrors: Record<string, string> = {};

    if (!form.email.trim()) {
      newErrors.email = "Email is required";
    }

    if (!form.password) {
      newErrors.password = "Password is required";
    }

    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!validate()) return;

    // Trim form fields before submission
    const trimmedForm = {
      email: form.email.trim(),
      password: form.password.trim(),
    };

    login.mutate(
      { data: trimmedForm, rememberMe },
      {
        onSuccess: (response) => {
          // Check user status and redirect accordingly
          // ACC-022: legacy (`pendingverification`) and canonical (`pending_verification`) spellings.
          const status = normalizeAccountStatus(response.user.status);
          if (status === "pending_verification") {
            router.push(
              `/verify?email=${encodeURIComponent(trimmedForm.email)}`,
            );
          } else if (status === "active") {
            router.push(returnPath());
          } else {
            // Handle other statuses (e.g., inactive)
            setErrors({
              general: "Account is not active. Please contact support.",
            });
          }
        },
        onError: (error) => {
          const axiosError = error as AxiosError<ApiErrorResponse>;
          const message =
            axiosError.response?.data?.error?.message ||
            "Login failed. Please try again.";
          setErrors({ general: message });
        },
      },
    );
  };

  return (
    <div className="relative z-10 bg-card rounded-2xl sm:rounded-[40px] shadow-2xl w-full max-w-[520px] px-5 py-6 sm:p-8 md:p-12 my-4 sm:my-6">
      {/* Header */}
      <AuthHeader title="Welcome Back" subtitle="Log in to your account." />

      {notice && !errors.general && (
        <div role="status" className="bg-muted border border-border text-sm rounded-lg px-4 py-2 mb-3">
          {notice}
        </div>
      )}

      {/* General Error */}
      {errors.general && (
        <div className="bg-red-50 border border-red-200 text-red-600 text-sm rounded-lg px-4 py-2 mb-3">
          {errors.general}
        </div>
      )}

      {/* Form Fields */}
      <form className="space-y-3" onSubmit={handleSubmit}>
        <AuthInput
          label="Email"
          type="email"
          placeholder="you@example.com"
          value={form.email}
          onChange={handleChange("email")}
          error={errors.email}
        />
        <AuthInput
          label="Password"
          type="password"
          placeholder="Enter your password"
          value={form.password}
          onChange={handleChange("password")}
          error={errors.password}
        />

        {/* Forgot Password */}
        <div className="flex justify-end">
          <Link
            href="/forgot-password"
            className="text-sm text-primary font-semibold hover:underline"
          >
            Forgot Password?
          </Link>
        </div>

        {/* Remember Me Checkbox */}
        <div className="flex items-center gap-3 py-1">
          <Checkbox
            id="remember-me"
            checked={rememberMe}
            onCheckedChange={(checked) => setRememberMe(checked === true)}
          />
          <label
            htmlFor="remember-me"
            className="text-sm text-primary cursor-pointer"
          >
            Remember Me
          </label>
        </div>

        {/* Main Log In Button */}
        <Button
          type="submit"
          size="lg"
          className="w-full"
          loading={login.isPending}
        >
          Log In
        </Button>
      </form>

      {/* Signup Redirect */}
      <div className="text-center md:mt-2 text-[12px] md:text-[14px] space-x-1">
        <p>
          Don&apos;t have an account?{" "}
          <Link href="/signup">
            <Button variant="link" className="p-1 text-[14px] md:text-[16px]">
              Sign Up
            </Button>
          </Link>
        </p>
      </div>

      {/* Divider */}
      <Divider text="OR" />

      {/* Google Sign In */}
      <Button
        type="button"
        variant="outline"
        className="w-full border-border text-muted-foreground hover:bg-muted rounded-2xl group"
        onClick={authService.initiateGoogleOAuth}
      >
        <div className="relative w-5 h-5 group-hover:scale-110 transition-transform">
          <Image
            src="/GoogleLogo.png"
            alt="Google"
            fill
            className="object-contain"
          />
        </div>
        Continue with Google
      </Button>
    </div>
  );
};

export default LoginModal;
