"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
import { useProfile } from "@/hooks";
import { Spinner } from "@/components/ui/spinner";
import AppSidebar from "./AppSidebar";
import MobileSidebar from "./MobileSidebar";

export default function DashboardLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const router = useRouter();
  const { data: user, isLoading, error } = useProfile();

  useEffect(() => {
    // If not loading and no user (either error like 401 Guest, or just no session), redirect
    if (!isLoading && (!user || error)) {
      router.push("/login");
    }
  }, [user, isLoading, error, router]);

  if (isLoading) {
    return (
      <div className="flex h-screen w-full items-center justify-center bg-white">
        <Spinner size="lg" className="text-[#029CD4]" />
      </div>
    );
  }

  // If we have an error or no user, render nothing while redirecting
  if (!user || error) {
    return null;
  }

  return (
    <div className="flex h-screen bg-white overflow-hidden">
      {/* Desktop sidebar */}
      <div className="hidden md:block">
        <AppSidebar />
      </div>

      <main className="flex-1 bg-white overflow-y-auto">{children}</main>

      {/* Mobile slide-in sidebar */}
      <MobileSidebar />
    </div>
  );
}
