"use client";

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
import type { SessionRecord } from "@/types/generated/meeting-service";

/** Settings → Security: password change (forced re-login) and account sessions (ACC-007). */
export default function AccountSecuritySettings() {
  const profile = useAccountProfile();
  const passwordAccount = profile.data ? profile.data.authType.toLowerCase() !== "google" : true;
  return (
    <div className="space-y-6">
      {passwordAccount && <ChangePassword />}
      <Sessions />
    </div>
  );
}

function PasswordInput({ id, label, value, onChange, autoComplete }: { id: string; label: string; value: string; onChange: (v: string) => void; autoComplete: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="space-y-1.5">
      <Label htmlFor={id}>{label}</Label>
      <div className="relative">
        <Input id={id} type={shown ? "text" : "password"} value={value} autoComplete={autoComplete} onChange={(e) => onChange(e.target.value)} className="h-11 rounded-xl pr-11" />
        <button
          type="button"
          aria-label={shown ? `Hide ${label}` : `Show ${label}`}
          onClick={() => setShown(!shown)}
          className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground"
        >
          {shown ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
        </button>
      </div>
    </div>
  );
}

function ChangePassword() {
  const router = useRouter();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    const problem = !current
      ? "Enter your current password."
      : passwordProblem(next) ?? (next !== confirm ? "The new passwords do not match." : next === current ? "Choose a password different from the current one." : null);
    if (problem) {
      setError(problem);
      return;
    }
    setPending(true);
    try {
      await accountService.changePassword({ current_password: current, new_password: next, confirm_password: confirm });
      // The API ended every session, this one included (ADR 0057): sign in again.
      await authService.forgetLocalSession();
      router.replace("/login?reason=password_changed");
    } catch (cause) {
      setError(apiErrorOf(cause).message ?? "Your password could not be changed.");
      setPending(false);
    } finally {
      setCurrent("");
    }
  };

  return (
    <div className="space-y-5 rounded-2xl border border-border bg-card p-5 sm:p-6">
      <div>
        <h2 className="text-base font-semibold">Password</h2>
        <p className="mt-0.5 text-sm text-muted-foreground">Changing it signs you out everywhere, including here.</p>
      </div>
      {error && <div role="alert" className="rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</div>}
      <form onSubmit={(e) => void submit(e)} className="max-w-md space-y-4">
        <PasswordInput id="current-password" label="Current password" value={current} onChange={setCurrent} autoComplete="current-password" />
        <PasswordInput id="new-password" label="New password" value={next} onChange={setNext} autoComplete="new-password" />
        <ul className="grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs">
          {RULES.map(([label, test]) => {
            const ok = test(next);
            return <li key={label} className={cn("flex items-center gap-1.5", ok ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground")}><span className={cn("flex size-4 items-center justify-center rounded-full", ok ? "bg-emerald-500 text-white" : "ring-1 ring-border")}>{ok && <Check className="size-3" strokeWidth={3} />}</span>{label}</li>;
          })}
        </ul>
        <PasswordInput id="confirm-password" label="Confirm new password" value={confirm} onChange={setConfirm} autoComplete="new-password" />
        {confirm && next !== confirm ? <p className="text-xs text-destructive">The passwords don&apos;t match yet.</p> : null}
        <Button type="submit" className="rounded-full" loading={pending} disabled={pending || !current || !next || !confirm}>
          Change password
        </Button>
      </form>
    </div>
  );
}

const RULES: [string, (v: string) => boolean][] = [
  ["8 or more characters", (v) => v.length >= 8 && v.length <= 72],
  ["A capital letter", (v) => /[A-Z]/.test(v)],
  ["A small letter", (v) => /[a-z]/.test(v)],
  ["A number", (v) => /\d/.test(v)],
  ["A symbol, like ! or #", (v) => /[^A-Za-z0-9]/.test(v)],
];

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
      await authService.logoutEverywhere();
      router.replace("/login?reason=signed_out_everywhere");
    } catch (cause) {
      setError(apiErrorOf(cause).message ?? "Could not sign out everywhere. Try again.");
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
            <button type="button" onClick={() => void revokeAll()} className="h-9 rounded-full bg-destructive px-4 text-sm font-semibold text-white hover:bg-destructive/90">Sign out everywhere</button>
          </div>
        ) : (
          <button type="button" disabled={everywhere === "pending"} onClick={() => setEverywhere("confirm")} className="inline-flex h-9 items-center gap-1.5 rounded-full px-4 text-sm font-semibold text-destructive ring-1 ring-destructive/30 hover:bg-destructive/10 disabled:opacity-50"><LogOut className="size-4" />{everywhere === "pending" ? "Signing out…" : "Sign out everywhere"}</button>
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
                    <span className="truncate">{s.clientLabel || "Unknown device"}</span>
                    {s.current && <span className="shrink-0 rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] font-semibold text-emerald-700 dark:text-emerald-400">This device</span>}
                  </p>
                  <p className="truncate text-xs text-muted-foreground">{s.current ? "You're using it now" : ago(s.lastRefreshedAt ?? s.createdAt)} · signed in with {s.authMethod?.toLowerCase() === "google" ? "Google" : "password"}</p>
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
