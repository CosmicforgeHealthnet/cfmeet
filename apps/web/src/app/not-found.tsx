"use client";

import React from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { Home, ArrowLeft, LogIn, UserPlus, KeyRound } from "lucide-react";

export default function NotFound() {
  const router = useRouter();

  return (
    <div className="min-h-screen flex items-center justify-center bg-gray-50 px-4 py-8">
      <div className="bg-white rounded-2xl sm:rounded-3xl shadow-xl w-full max-w-lg px-6 py-8 sm:px-10 sm:py-10 text-center">
        {/* 404 Number */}
        <h1 className="text-7xl sm:text-8xl font-bold bg-gradient-to-r from-[#029CD4] to-[#0284c7] bg-clip-text text-transparent mb-4">
          404
        </h1>

        {/* Title */}
        <h2 className="text-xl sm:text-2xl font-bold text-gray-900 mb-2">
          Page Not Found
        </h2>

        {/* Description */}
        <p className="text-gray-600 text-sm sm:text-base mb-6">
          The page you&apos;re looking for doesn&apos;t exist or has been moved.
        </p>

        {/* Action Buttons */}
        <div className="flex flex-col sm:flex-row gap-3 justify-center mb-6">
          <Button
            size="lg"
            onClick={() => router.back()}
            variant="outline"
            className="w-full sm:w-auto"
          >
            <ArrowLeft className="w-4 h-4 mr-2" />
            Go Back
          </Button>
          <Link href="/" className="w-full sm:w-auto">
            <Button size="lg" className="w-full">
              <Home className="w-4 h-4 mr-2" />
              Go Home
            </Button>
          </Link>
        </div>

        {/* Quick Links */}
        <div className="pt-6 border-t border-gray-200">
          <p className="text-xs text-gray-500 mb-3">Quick Links</p>
          <div className="flex flex-wrap gap-4 justify-center">
            <Link href="/login">
              <Button variant="ghost" size="sm" className="text-[#029CD4]">
                <LogIn className="w-4 h-4 mr-1.5" />
                Login
              </Button>
            </Link>
            <Link href="/register">
              <Button variant="ghost" size="sm" className="text-[#029CD4]">
                <UserPlus className="w-4 h-4 mr-1.5" />
                Register
              </Button>
            </Link>
            <Link href="/forgot-password">
              <Button variant="ghost" size="sm" className="text-[#029CD4]">
                <KeyRound className="w-4 h-4 mr-1.5" />
                Reset Password
              </Button>
            </Link>
          </div>
        </div>
      </div>
    </div>
  );
}
