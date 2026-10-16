"use client";

import { Suspense } from "react";
import { useGoogleCallback } from "@/hooks";
import { Spinner } from "@/components/ui/spinner";

function GoogleCallbackContent() {
  const { error } = useGoogleCallback();

  if (error) {
    // ... (keep error handling)
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-background">
      <div className="text-center">
        <Spinner size="lg" className="mb-4" />
        <p className="text-sm text-muted-foreground">Signing you in...</p>
      </div>
    </div>
  );
}

export default function GoogleCallbackPage() {
  return (
    <Suspense
      fallback={
        <div className="min-h-screen flex items-center justify-center bg-background">
          <Spinner size="lg" />
        </div>
      }
    >
      <GoogleCallbackContent />
    </Suspense>
  );
}
