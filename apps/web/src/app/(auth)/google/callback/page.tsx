"use client";

import { Suspense } from "react";
import Link from "next/link";
import { useGoogleCallback } from "@/hooks";
import { Spinner } from "@/components/ui/spinner";

function GoogleCallbackContent() {
  const { error } = useGoogleCallback();

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background p-6">
        <div role="alert" className="max-w-md space-y-4 text-center">
          <p className="text-sm text-destructive">{error}</p>
          <Link className="text-sm font-medium text-primary underline" href="/login">
            Back to sign in
          </Link>
        </div>
      </div>
    );
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
