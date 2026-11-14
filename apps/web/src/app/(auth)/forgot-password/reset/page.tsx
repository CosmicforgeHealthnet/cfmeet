"use client";

import React, { Suspense } from "react";
import { useSearchParams, useRouter } from "next/navigation";
import PasswordResetModal from "@/components/auth/PasswordResetModal";

function ResetPasswordContent() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const email = searchParams.get("email");
  const token = searchParams.get("token");

  // If email or token is missing, redirect to forgot password
  if (!email || !token) {
    router.push("/forgot-password");
    return null;
  }

  return (
    <PasswordResetModal email={email} token={token} />
  );
}

const ResetPasswordPage = () => {
  return (
    <Suspense
      fallback={
        <p className="text-muted-foreground">Loading…</p>
      }
    >
      <ResetPasswordContent />
    </Suspense>
  );
};

export default ResetPasswordPage;
