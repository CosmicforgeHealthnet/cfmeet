"use client";

import React, { useState, useEffect, useSyncExternalStore } from "react";
import Link from "next/link";
import Image from "next/image";
import { useRouter } from "next/navigation";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import { Checkbox } from "../ui/checkbox";
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
    <div>
      <h1 className="text-3xl font-bold tracking-tight text-foreground">Welcome back</h1>
      <p className="mt-2 text-base text-muted-foreground">Sign in to your CF Meet account.</p>

      <Button
        type="button"
        variant="outline"
        className="mt-6 h-12 w-full rounded-xl border-border bg-background font-semibold text-foreground shadow-none hover:bg-muted"
        onClick={authService.initiateGoogleOAuth}
      >
        <Image src="/GoogleLogo.png" alt="" width={20} height={20} className="size-5" />
        Continue with Google
      </Button>

      <div className="my-5 flex items-center gap-3 text-xs uppercase tracking-wider text-muted-foreground">
        <span className="h-px flex-1 bg-border" />
        or
        <span className="h-px flex-1 bg-border" />
      </div>

      {notice && !errors.general && (
        <div role="status" className="mb-4 rounded-xl border border-border bg-muted px-4 py-3 text-sm">
          {notice}
        </div>
      )}

      {errors.general && (
        <div role="alert" className="mb-4 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {errors.general}
        </div>
      )}

      <form className="space-y-5" onSubmit={handleSubmit} noValidate>
        <AuthInput
          label="Email"
          type="email"
          autoComplete="email"
          placeholder="you@example.com"
          value={form.email}
          onChange={handleChange("email")}
          error={errors.email}
        />
        <AuthInput
          label="Password"
          type="password"
          autoComplete="current-password"
          placeholder="Enter your password"
          value={form.password}
          onChange={handleChange("password")}
          error={errors.password}
        />

        <div className="flex items-center justify-between">
          <label htmlFor="remember-me" className="flex cursor-pointer items-center gap-2 text-sm text-foreground">
            <Checkbox
              id="remember-me"
              checked={rememberMe}
              onCheckedChange={(checked) => setRememberMe(checked === true)}
            />
            Remember me
          </label>
          <Link href="/forgot-password" className="text-sm font-semibold text-primary hover:underline">
            Forgot password?
          </Link>
        </div>

        <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" loading={login.isPending}>
          Sign in
        </Button>
      </form>

      <p className="mt-6 text-center text-sm text-muted-foreground">
        New to CF Meet?{" "}
        <Link href="/signup" className="font-semibold text-primary hover:underline">
          Create an account
        </Link>
      </p>

      <p className="mt-6 border-t border-border pt-5 text-center text-sm text-muted-foreground">
        Joining a meeting?{" "}
        <Link href="/join" className="font-semibold text-foreground hover:text-primary hover:underline">
          Join as a guest
        </Link>
      </p>
    </div>
  );
};

export default LoginModal;
