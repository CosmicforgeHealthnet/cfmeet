"use client";

import React, { useState } from "react";
import Link from "next/link";
import Image from "next/image";
import { useRouter } from "next/navigation";
import AuthInput from "@/components/auth/AuthInput";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
import { useRegister } from "@/hooks";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { authService } from "@/services/auth.service";
import { Check } from "lucide-react";

/** Mirrors the server's password rules (accepted symbols: !@#$%^&*(),.?:{}|<>). */
const SYMBOLS = /[!@#$%^&*(),.?:{}|<>]/;
const PASSWORD_RULES: [string, (v: string) => boolean][] = [
  ["8+ characters", (v) => v.length >= 8],
  ["A capital letter", (v) => /[A-Z]/.test(v)],
  ["A small letter", (v) => /[a-z]/.test(v)],
  ["A number", (v) => /\d/.test(v)],
  ["A symbol like ! @ # ?", (v) => SYMBOLS.test(v)],
];

const SignupModal = () => {
  const router = useRouter();
  const register = useRegister();

  const [form, setForm] = useState({
    username: "",
    email: "",
    password: "",
    confirm_password: "",
  });
  const [agreed, setAgreed] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});

  const handleChange =
    (field: string) => (e: React.ChangeEvent<HTMLInputElement>) => {
      setForm((prev) => ({ ...prev, [field]: e.target.value }));
      setErrors((prev) => ({ ...prev, [field]: "", general: "" }));
    };

  const validate = () => {
    const newErrors: Record<string, string> = {};

    if (!form.username.trim()) {
      newErrors.username = "Username is required";
    } else if (form.username.length < 3 || form.username.length > 50) {
      newErrors.username = "Username must be between 3 and 50 characters";
    } else if (!/^[a-zA-Z0-9_]+$/.test(form.username)) {
      newErrors.username =
        "Username can only contain letters, numbers, and underscores";
    }

    if (!form.email.trim()) {
      newErrors.email = "Email is required";
    } else if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(form.email)) {
      newErrors.email = "Invalid email address";
    }

    if (!form.password) {
      newErrors.password = "Password is required";
    } else if (!PASSWORD_RULES.every(([, test]) => test(form.password))) {
      newErrors.password = "Your password doesn't meet all the rules below yet.";
    }


    if (!agreed) {
      newErrors.terms = "Please agree to the Terms of Service and Privacy Policy";
    }

    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!validate()) return;

    // Trim all form fields before submission
    const trimmedForm = {
      username: form.username.trim(),
      email: form.email.trim(),
      password: form.password.trim(),
      // One password field with show/hide (modern pattern); the API still takes a confirmation.
      confirm_password: form.password.trim(),
    };

    register.mutate(trimmedForm, {
      onSuccess: () => {
        router.push(`/verify?email=${encodeURIComponent(trimmedForm.email)}`);
      },
      onError: (error) => {
        const axiosError = error as AxiosError<ApiErrorResponse>;
        const raw = axiosError.response?.data?.error?.message || "Registration failed. Please try again.";
        // Server messages can carry a field prefix like "__all__: " or "password: ".
        const message = raw.replace(/^[_a-z]+:\s*/i, "");
        setErrors(/password/i.test(raw) ? { password: message } : { general: message });
      },
    });
  };


  return (
    <div>
      <h1 className="text-3xl font-bold tracking-tight text-foreground">Create your account</h1>
      <p className="mt-2 text-base text-muted-foreground">Meet, call and message with CF Meet. It&apos;s free.</p>

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

      {errors.general && (
        <div role="alert" className="mb-4 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {errors.general}
        </div>
      )}

      <form className="space-y-4" onSubmit={handleSubmit} noValidate>
        <AuthInput
          label="Username"
          type="text"
          autoComplete="nickname"
          placeholder="e.g. ada_nwosu"
          value={form.username}
          onChange={handleChange("username")}
          error={errors.username}
        />
        <AuthInput
          label="Email"
          type="email"
          autoComplete="email"
          placeholder="you@example.com"
          value={form.email}
          onChange={handleChange("email")}
          error={errors.email}
        />
        <div className="space-y-2">
          <AuthInput
            label="Password"
            type="password"
            autoComplete="new-password"
            placeholder="Create a password"
            value={form.password}
            onChange={handleChange("password")}
            error={errors.password}
          />
          <ul className="grid grid-cols-2 gap-x-3 gap-y-1.5 pt-1 text-sm" aria-label="Password rules">
            {PASSWORD_RULES.map(([label, test]) => {
              const ok = test(form.password);
              return (
                <li key={label} className={`flex items-center gap-1.5 ${ok ? "text-[var(--success)]" : "text-muted-foreground"}`}>
                  <Check className={`size-4 shrink-0 ${ok ? "" : "opacity-40"}`} aria-hidden />{label}
                </li>
              );
            })}
          </ul>
        </div>

        <div className="space-y-1">
          <label htmlFor="terms" className="flex cursor-pointer items-start gap-2.5 text-sm text-muted-foreground">
            <Checkbox
              id="terms"
              className="mt-0.5"
              checked={agreed}
              onCheckedChange={(checked) => {
                setAgreed(checked === true);
                setErrors((prev) => ({ ...prev, terms: "" }));
              }}
            />
            <span>
              I agree to the{" "}
              <Link href="/terms" className="font-semibold text-primary hover:underline">Terms of Service</Link> and{" "}
              <Link href="/privacy" className="font-semibold text-primary hover:underline">Privacy Policy</Link>
            </span>
          </label>
          {errors.terms && <p className="text-sm text-destructive">{errors.terms}</p>}
        </div>

        <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none hover:brightness-100" loading={register.isPending}>
          Create account
        </Button>
      </form>

      <p className="mt-6 text-center text-sm text-muted-foreground">
        Already have an account?{" "}
        <Link href="/login" className="font-semibold text-primary hover:underline">
          Sign in
        </Link>
      </p>
    </div>
  );
};

export default SignupModal;
