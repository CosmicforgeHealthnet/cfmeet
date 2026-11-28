"use client";

import { useEffect, useSyncExternalStore } from "react";
import { useRouter } from "next/navigation";
import { SessionEndedError } from "@/lib/auth/session-core";
import { MessagingEngineProvider } from "@/components/messaging/MessagingEngineProvider";
import { ChatAlerts } from "@/components/messaging/ChatAlerts";
import { StatusQueueRoot } from "@/components/updates/StatusQueueRoot";
import { StatusViewerHost } from "@/components/updates/StatusRing";
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

/** A profile error that means the session is over (not a server or network problem). */
function isSignedOutError(error: unknown): boolean {
  if (error instanceof SessionEndedError) return true;
  const status = (error as { response?: { status?: number } } | null)?.response?.status;
  return status === 401;
}

export default function DashboardLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const router = useRouter();
  const { data: user, isLoading, error, refetch, isFetching } = useProfile();
  // Only a real sign-out goes back to /login; a server or network error offers a retry
  // (redirecting on those bounced between /login and here, since the session is still there).
  const signedOut = !!error && isSignedOutError(error);
  const mounted = useSyncExternalStore(subscribeToHydration, getHydrated, getServerHydrated);
  useSessionWatch();

  useEffect(() => {
    // If not loading and no user (either error like 401 Guest, or just no session), redirect
    if (mounted && !isLoading && ((!user && !error) || signedOut)) {
      router.replace(`/login?next=${encodeURIComponent(window.location.pathname + window.location.search)}`);
    }
  }, [user, isLoading, error, signedOut, router, mounted]);

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

  if (error && !signedOut) {
    return (
      <div className="flex h-screen w-full flex-col items-center justify-center gap-3 bg-background px-6 text-center" data-testid="app-unreachable">
        <p className="text-lg font-semibold">Couldn&apos;t reach CF Meet</p>
        <p className="max-w-sm text-sm text-muted-foreground">Check your connection. You&apos;re still signed in.</p>
        <button type="button" onClick={() => void refetch()} disabled={isFetching}
          className="mt-2 h-10 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60">
          {isFetching ? "Trying…" : "Try again"}
        </button>
      </div>
    );
  }

  // No user (signed out): render nothing while redirecting
  if (!user || error) {
    return null;
  }

  return (
    <WorkspaceProvider>
     <MessagingEngineProvider>
     <StatusViewerHost>
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
        <ChatAlerts />
        {/* Background status publishing (WhatsApp-style): resumes pending statuses, runs across pages. */}
        <StatusQueueRoot ownerId={user.id} />
      </div>
     </StatusViewerHost>
     </MessagingEngineProvider>
    </WorkspaceProvider>
  );
}
