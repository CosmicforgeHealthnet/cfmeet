"use client";

import React from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Home, RefreshCw, AlertTriangle } from "lucide-react";

export default function ServerError() {

  const handleRetry = () => {
    // Hard reload to try to recover from potential state/cache issues or server recovery
    window.location.reload();
  };

  return (
    <div className="min-h-screen flex items-center justify-center bg-gray-50 px-4 py-8">
      <div className="bg-white rounded-2xl sm:rounded-3xl shadow-xl w-full max-w-lg px-6 py-8 sm:px-10 sm:py-10 text-center">
        {/* Error Icon/Number */}
        <div className="mx-auto w-24 h-24 bg-red-50 rounded-full flex items-center justify-center mb-6">
          <AlertTriangle className="w-12 h-12 text-red-500" />
        </div>

        <h1 className="text-4xl sm:text-5xl font-bold text-gray-900 mb-2">
          500
        </h1>

        {/* Title */}
        <h2 className="text-xl sm:text-2xl font-bold text-gray-900 mb-4">
          Internal Server Error
        </h2>

        {/* Description */}
        <p className="text-gray-600 text-sm sm:text-base mb-8">
          Oops! Something went wrong on our end. We&apos;re working to fix it. Please
          try again later.
        </p>

        {/* Action Buttons */}
        <div className="flex flex-col sm:flex-row gap-3 justify-center mb-6">
          <Button
            size="lg"
            onClick={handleRetry}
            className="w-full sm:w-auto bg-[#029CD4] hover:bg-[#0284c7]"
          >
            <RefreshCw className="w-4 h-4 mr-2" />
            Try Again
          </Button>
          <Link href="/" className="w-full sm:w-auto">
            <Button size="lg" variant="outline" className="w-full">
              <Home className="w-4 h-4 mr-2" />
              Go Home
            </Button>
          </Link>
        </div>

        {/* Support Link or Footer */}
        <div className="pt-6 border-t border-gray-100">
          <p className="text-xs text-gray-400">
            If the issue persists, please contact{" "}
            <a
              href="mailto:support@cosmicforge.com"
              className="text-[#029CD4] hover:underline"
            >
              support
            </a>
            .
          </p>
        </div>
      </div>
    </div>
  );
}
