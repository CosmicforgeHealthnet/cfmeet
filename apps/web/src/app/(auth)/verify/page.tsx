"use client";

import { useSearchParams } from "next/navigation";
import { Suspense } from "react";
import VerifyModal from "@/components/auth/VerifyModal";
import { authService } from "@/services/auth.service";

function VerifyContent() {
  const searchParams = useSearchParams();
  const email = searchParams.get("email") || "";

  const handleResend = async () => {
    if (!email) {
      throw new Error("Email is missing");
    }
    await authService.resendVerification({ email });
  };

  return <VerifyModal email={email} onResend={handleResend} />;
}

export default function VerifyPage() {
  return (
    <Suspense>
      <VerifyContent />
    </Suspense>
  );
}
