"use client";

import TwoStepCard from "./TwoStepCard";
import { useState } from "react";
import { useRouter } from "next/navigation";
import { Check, Eye, EyeOff, Globe, Laptop, LogOut, Smartphone } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useAccountProfile, useAccountSessions, useRevokeAccountSession } from "@/hooks/useAccount";
import { accountService } from "@/services/account.service";
import { authService } from "@/services/auth.service";
import { apiErrorOf, passwordProblem } from "@/lib/account/account-core";
import { validationMessageOf } from "@/lib/validation-errors";
import { browserDeviceLabel } from "@/lib/auth/device-label";

/** "Chrome on Android" instead of the raw browser string the session recorded. */
const sessionLabel = (label: string | null | undefined) => !label ? "Unknown device" : /Mozilla\/|AppleWebKit|Gecko\//.test(label) ? browserDeviceLabel(label) : label;
/** Sign-in is passwordless: an email (or text) code, or Google. */
const signedInWith = (method: string | null | undefined) => {
  const m = (method ?? "").toLowerCase();
  return m.includes("google") ? "signed in with Google" : m.includes("phone") || m.includes("sms") ? "signed in with a text code" : "signed in with an email code";
};
import type { SessionRecord } from "@/types/generated/meeting-service";

/** Settings → Security: two-step sign-in and where you're signed in (ACC-007). */
export default function AccountSecuritySettings() {
  return <div className="space-y-4"><TwoStepCard /><Sessions /></div>;
}

function ago(value: string | null | undefined) {
  if (!value) return "—";
  const mins = Math.round((Date.now() - Date.parse(value)) / 60000);
  if (mins < 2) return "Active now";
  if (mins < 60) return `Active ${mins} min ago`;
  if (mins < 24 * 60) return `Active ${Math.round(mins / 60)} h ago`;
  return `Active ${new Date(value).toLocaleDateString([], { day: "numeric", month: "short" })}`;
}
const deviceIcon = (label?: string | null) => (/iphone|android|mobile|ios/i.test(label ?? "") ? Smartphone : /mac|windows|linux|chrome|edge|firefox|safari/i.test(label ?? "") ? Laptop : Globe);

function Sessions() {
  const router = useRouter();
  const sessions = useAccountSessions();
  const revoke = useRevokeAccountSession();
  const [everywhere, setEverywhere] = useState<"idle" | "confirm" | "pending">("idle");
  const [error, setError] = useState("");

  const revokeOne = async (session: SessionRecord) => {
    setError("");
    try {
      await revoke.mutateAsync(session.sessionId);
      if (session.current) {
        await authService.forgetLocalSession();
        router.replace("/login?reason=session_ended");
      }
    } catch (cause) {
      setError(apiErrorOf(cause).message ?? "The session could not be signed out.");
    }
  };

  const revokeAll = async () => {
    setEverywhere("pending");
    setError("");
    try {
      // WhatsApp "Log out from all other devices": this device stays signed in.
      await accountService.revokeOtherSessions();
      setEverywhere("idle");
      void sessions.refetch();
    } catch (cause) {
      setError(apiErrorOf(cause).message ?? "Could not sign out your other devices. Try again.");
      setEverywhere("idle");
    }
  };

  const active = (sessions.data ?? []).filter((s) => s.status === "ACTIVE");

  return (
    <div className="space-y-4 rounded-2xl border border-border bg-card p-5 sm:p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold">Where you&apos;re signed in</h2>
          <p className="mt-0.5 text-sm text-muted-foreground">Sign out anything you don&apos;t recognise.</p>
        </div>
        {everywhere === "confirm" ? (
          <div className="flex gap-2">
            <button type="button" onClick={() => setEverywhere("idle")} className="h-9 rounded-full px-4 text-sm font-semibold hover:bg-muted">Cancel</button>
            <button type="button" onClick={() => void revokeAll()} className="h-9 rounded-xl bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90">Sign out other devices</button>
          </div>
        ) : (
          <button type="button" disabled={everywhere === "pending"} onClick={() => setEverywhere("confirm")} className="inline-flex h-9 items-center gap-1.5 rounded-full px-4 text-sm font-semibold text-destructive ring-1 ring-destructive/30 hover:bg-destructive/10 disabled:opacity-50"><LogOut className="size-4" />{everywhere === "pending" ? "Signing out…" : "Sign out other devices"}</button>
        )}
      </div>
      {error && <p role="alert" className="rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p>}
      {sessions.isLoading && <div className="space-y-2">{[0, 1].map((i) => <div key={i} className="h-16 animate-pulse rounded-xl bg-muted" />)}</div>}
      {sessions.isError && (
        <p role="alert" className="flex items-center gap-3 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">
          <span className="flex-1">Your devices couldn&apos;t load.</span>
          <button type="button" className="font-semibold hover:underline" onClick={() => void sessions.refetch()}>Try again</button>
        </p>
      )}
      {sessions.data && active.length === 0 && <p className="text-sm text-muted-foreground">No active sessions.</p>}
      {active.length > 0 && (
        <ul className="divide-y divide-border">
          {[...active].sort((a, b) => Number(b.current) - Number(a.current)).map((s) => {
            const Icon = deviceIcon(s.clientLabel);
            return (
              <li key={s.sessionId} className="flex items-center gap-3 py-3">
                <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-xl", s.current ? "bg-primary/10 text-primary" : "bg-muted text-muted-foreground")}><Icon className="size-5" /></span>
                <div className="min-w-0 flex-1">
                  <p className="flex items-center gap-2 truncate text-sm font-semibold">
                    <span className="truncate">{sessionLabel(s.clientLabel)}</span>
                    {s.current && <span className="shrink-0 rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] font-semibold text-emerald-700 dark:text-emerald-400">This device</span>}
                  </p>
                  <p className="truncate text-xs text-muted-foreground">{s.current ? "You're using it now" : ago(s.lastRefreshedAt ?? s.createdAt)} · {signedInWith(s.authMethod)}</p>
                </div>
                <button type="button" disabled={revoke.isPending} onClick={() => void revokeOne(s)} className="h-9 shrink-0 rounded-full px-3.5 text-sm font-semibold text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50">Sign out</button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
