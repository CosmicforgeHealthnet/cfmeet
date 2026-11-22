"use client";

import { useEffect, useSyncExternalStore } from "react";
import { useRouter } from "next/navigation";
import { useProfile } from "@/hooks";
import { Spinner } from "@/components/ui/spinner";
import AppSidebar from "./AppSidebar";
import MobileSidebar from "./MobileSidebar";
import TopBar from "./TopBar";
import MobileTabs from "./MobileTabs";
import CommandPalette from "./CommandPalette";
import { Toaster } from "@/components/ui/toast";
import { WorkspaceProvider } from "@/context/WorkspaceContext";
import { useSessionWatch } from "@/lib/auth/use-session-watch";

const subscribeToHydration = () => () => {};
const getHydrated = () => true;
const getServerHydrated = () => false;

export default function DashboardLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const router = useRouter();
  const { data: user, isLoading, error } = useProfile();
  const mounted = useSyncExternalStore(subscribeToHydration, getHydrated, getServerHydrated);
  useSessionWatch();

  useEffect(() => {
    // If not loading and no user (either error like 401 Guest, or just no session), redirect
    if (mounted && !isLoading && (!user || error)) {
      router.push("/login");
    }
  }, [user, isLoading, error, router, mounted]);

  // Show nothing during SSR to prevent hydration mismatch
  if (!mounted) {
    return null;
  }

  if (isLoading) {
    return (
      <div className="flex h-screen w-full items-center justify-center bg-background">
        <Spinner size="lg" className="text-primary" />
      </div>
    );
  }

  // If we have an error or no user, render nothing while redirecting
  if (!user || error) {
    return null;
  }

  return (
    <WorkspaceProvider>
      <div className="flex h-screen overflow-hidden bg-background">
        {/* Desktop / tablet rail */}
        <div className="hidden md:block">
          <AppSidebar />
        </div>

        <div className="flex min-w-0 flex-1 flex-col">
          <TopBar />
          <main className="flex-1 overflow-y-auto bg-muted/40 pb-20 md:pb-0 dark:bg-background">{children}</main>
        </div>

        {/* Phone: bottom tabs + full menu drawer behind "More" */}
        <MobileTabs />
        <MobileSidebar />
        <CommandPalette />
        <Toaster />
      </div>
    </WorkspaceProvider>
  );
}
