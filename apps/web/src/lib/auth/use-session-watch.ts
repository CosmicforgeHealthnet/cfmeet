"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import { accountSession } from "./session";
import { storageStore } from "@/store";

/**
 * Signed-in shells: when the account session ends (refresh rejected — expired, revoked from
 * another device, password changed — or another tab signed out), drop every cached query of
 * the old session and return to sign-in. A sign-out started in this tab only clears the
 * cache; its caller navigates.
 */
export function useSessionWatch(): void {
  const router = useRouter();
  const queryClient = useQueryClient();
  useEffect(
    () =>
      accountSession.subscribe((signedIn, change) => {
        if (signedIn) return;
        queryClient.clear();
        if (change === "ended" || change === "signed_out_elsewhere") {
          storageStore.clearUser();
          router.replace("/login?reason=session_ended");
        }
      }),
    [queryClient, router],
  );
}
