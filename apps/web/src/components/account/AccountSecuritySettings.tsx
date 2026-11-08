"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Eye, EyeOff } from "lucide-react";
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
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <div className="relative">
        <Input id={id} type={shown ? "text" : "password"} value={value} autoComplete={autoComplete} onChange={(e) => onChange(e.target.value)} className="pr-10" />
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
    <div className="space-y-6 rounded-2xl border border-border bg-card p-6 shadow-sm">
      <div>
        <h2 className="mb-1 text-lg font-semibold">Change password</h2>
        <p className="text-sm text-muted-foreground">Changing your password signs you out on every device, including this one.</p>
      </div>
      {error && (
        <div role="alert" className="rounded-lg border border-red-100 bg-red-50 px-4 py-3 text-sm text-red-600">
          {error}
        </div>
      )}
      <form onSubmit={(e) => void submit(e)} className="max-w-xl space-y-4">
        <PasswordInput id="current-password" label="Current password" value={current} onChange={setCurrent} autoComplete="current-password" />
        <PasswordInput id="new-password" label="New password" value={next} onChange={setNext} autoComplete="new-password" />
        <p className="text-xs text-muted-foreground">8–72 characters with upper- and lowercase letters, a number and a special character.</p>
        <PasswordInput id="confirm-password" label="Confirm new password" value={confirm} onChange={setConfirm} autoComplete="new-password" />
        <Button type="submit" loading={pending} disabled={pending || !current || !next || !confirm}>
          Change password
        </Button>
      </form>
    </div>
  );
}

const formatTime = (value: string | null | undefined) => (value ? new Date(value).toLocaleString() : "—");

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
    <div className="space-y-4 rounded-2xl border border-border bg-card p-6 shadow-sm">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="mb-1 text-lg font-semibold">Where you are signed in</h2>
          <p className="text-sm text-muted-foreground">Sign out any session you do not recognise.</p>
        </div>
        {everywhere === "confirm" ? (
          <div className="flex gap-2">
            <Button size="sm" variant="destructive" onClick={() => void revokeAll()}>
              Confirm: sign out everywhere
            </Button>
            <Button size="sm" variant="outline" onClick={() => setEverywhere("idle")}>
              Cancel
            </Button>
          </div>
        ) : (
          <Button size="sm" variant="outline" loading={everywhere === "pending"} disabled={everywhere === "pending"} onClick={() => setEverywhere("confirm")}>
            Sign out everywhere
          </Button>
        )}
      </div>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      {sessions.isLoading && <p role="status" className="text-sm">Loading sessions…</p>}
      {sessions.isError && (
        <p role="alert" className="text-sm text-destructive">
          Sessions could not be loaded.{" "}
          <Button size="sm" variant="outline" onClick={() => void sessions.refetch()}>
            Retry
          </Button>
        </p>
      )}
      {sessions.data && active.length === 0 && <p className="text-sm text-muted-foreground">No active sessions.</p>}
      {active.length > 0 && (
        <ul className="divide-y divide-border rounded-md border border-border">
          {active.map((s) => (
            <li key={s.sessionId} className="flex flex-wrap items-center justify-between gap-3 p-3 text-sm">
              <div className="min-w-0">
                <p className="font-medium">
                  {s.clientLabel || "Unknown device"}
                  {s.current && <span className="ml-2 rounded bg-primary/10 px-1.5 py-0.5 text-xs text-primary">This browser</span>}
                </p>
                <p className="text-muted-foreground">
                  {s.authMethod} sign-in {formatTime(s.authenticatedAt)} · last active {formatTime(s.lastRefreshedAt ?? s.createdAt)} · ends by {formatTime(s.expiresAt)}
                </p>
              </div>
              <Button size="sm" variant="outline" disabled={revoke.isPending} onClick={() => void revokeOne(s)}>
                {s.current ? "Sign out" : "Sign out session"}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
