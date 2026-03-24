"use client";

import React, { useState } from "react";
import Link from "next/link";
import Image from "next/image";
import { useRouter } from "next/navigation";
import AuthInput from "@/components/auth/AuthInput";
import { Checkbox } from "@/components/ui/checkbox";
import { Button } from "@/components/ui/button";
import AuthHeader from "./AuthHeader";
import Divider from "./Divider";
import { useRegister } from "@/hooks";
import type { ApiErrorResponse } from "@/types/auth";
import { AxiosError } from "axios";
import { authService } from "@/services/auth.service";

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
    } else if (form.password.length < 8) {
      newErrors.password = "Password must be at least 8 characters";
    }

    if (form.password !== form.confirm_password) {
      newErrors.confirm_password = "Passwords do not match";
    }

    if (!agreed) {
      newErrors.terms = "You must accept the Terms & Conditions";
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
      confirm_password: form.confirm_password.trim(),
    };

    register.mutate(trimmedForm, {
      onSuccess: () => {
        router.push(`/verify?email=${encodeURIComponent(trimmedForm.email)}`);
      },
      onError: (error) => {
        const axiosError = error as AxiosError<ApiErrorResponse>;
        const message =
          axiosError.response?.data?.error?.message ||
          "Registration failed. Please try again.";
        setErrors({ general: message });
      },
    });
  };

  return (
    <div className="relative z-10 bg-white rounded-2xl sm:rounded-[40px] shadow-2xl w-full max-w-[520px] px-5 py-6 sm:p-8 md:p-12 my-4 sm:my-6">
      {/* Header */}
      <AuthHeader
        title="Create Your Account"
        subtitle="Welcome! Let's get you Started."
      />

      {/* General Error */}
      {errors.general && (
        <div className="bg-red-50 border border-red-200 text-red-600 text-sm rounded-lg px-4 py-2 mb-3">
          {errors.general}
        </div>
      )}

      {/* Form Fields */}
      <form className="space-y-3" onSubmit={handleSubmit}>
        <AuthInput
          label="Username"
          type="text"
          placeholder="Enter your preferred Username"
          value={form.username}
          onChange={handleChange("username")}
          error={errors.username}
        />
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
          placeholder="Create a Password"
          value={form.password}
          onChange={handleChange("password")}
          error={errors.password}
        />
        <AuthInput
          label="Confirm Password"
          type="password"
          placeholder="Confirm your password"
          value={form.confirm_password}
          onChange={handleChange("confirm_password")}
          error={errors.confirm_password}
        />

        {/* Terms Checkbox */}
        <div className="space-y-1">
          <div className="flex items-center gap-3 py-1">
            <Checkbox
              id="terms"
              checked={agreed}
              onCheckedChange={(checked) => {
                setAgreed(checked === true);
                setErrors((prev) => ({ ...prev, terms: "" }));
              }}
            />
            <label
              htmlFor="terms"
              className="text-sm text-gray-600 cursor-pointer"
            >
              I accept the{" "}
              <Link
                href="#"
                className="text-[#029CD4] font-semibold hover:underline"
              >
                Terms & Condition
              </Link>
            </label>
          </div>
          {errors.terms && (
            <p className="text-red-500 text-xs">{errors.terms}</p>
          )}
        </div>

        {/* Main Sign Up Button */}
        <Button
          type="submit"
          size="lg"
          className="w-full"
          loading={register.isPending}
        >
          Sign Up
        </Button>
      </form>

      {/* Login Redirect */}
      <div className="text-center md:mt-2 text-[12px] md:text-[14px] space-x-1">
        <p className="">
          Already have an account?
          <Link href="/login">
            <Button variant="link" className="p-1 text-[14px] md:text-[16px]">
              Log In
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
        className="w-full border-gray-100 text-gray-500 hover:bg-gray-50 rounded-2xl group"
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

export default SignupModal;
