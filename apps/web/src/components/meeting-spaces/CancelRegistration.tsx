"use client";

import { useState, useSyncExternalStore, type FormEvent } from "react";
import Link from "next/link";
import { CalendarX2, CheckCircle2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { profilesService } from "@/services/v2/profiles.service";
import { Field, RequestError } from "./ManagementUI";

/** The token from an admission link (`…/invite#reg_…`), a `#reg_…` fragment, or the bare token. */
export function registrationToken(input: string): string {
  const text = input.trim();
  const hash = text.lastIndexOf("#");
  const raw = hash >= 0 ? text.slice(hash + 1) : text;
  try { return decodeURIComponent(raw).trim(); } catch { return raw.trim(); }
}

const noSubscribe = () => () => {};
const fragment = () => (typeof window === "undefined" ? "" : window.location.hash);

/**
 * Cancel my webinar / town-hall registration (POST /registrations/cancel) with the admission
 * link I received. No sign-in: the link is the proof. The token is revoked, so it can no
 * longer be used to join.
 */
export default function CancelRegistration({ initialToken }: { initialToken?: string }) {
  const hash = useSyncExternalStore(noSubscribe, fragment, () => "");
  const [typed, setTyped] = useState<string | null>(null);
  const value = typed ?? initialToken ?? (hash ? registrationToken(hash) : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [done, setDone] = useState<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const token = registrationToken(value);
    if (!token) return;
    setBusy(true);
    setError(null);
    try {
      const view = await profilesService.cancelOwnRegistration(token);
      setDone(view.displayName ?? "");
      // The token is spent: it shouldn't stay in the address bar or history.
      if (typeof window !== "undefined" && window.location.hash) window.history.replaceState(null, "", window.location.pathname);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const notFound = (error as { response?: { data?: { error?: { code?: string } } } } | null)?.response?.data?.error?.code === "REGISTRATION_NOT_FOUND";

  return (
    <main className="mx-auto flex min-h-[70vh] max-w-md flex-col justify-center p-6">
      {done !== null ? (
        <div className="space-y-4 text-center" role="status" data-testid="registration-cancelled">
          <CheckCircle2 className="mx-auto size-12 text-primary" />
          <h1 className="text-2xl font-bold">Registration cancelled</h1>
          <p className="text-sm text-muted-foreground">{done ? `${done}, your` : "Your"} place has been released and your admission link no longer works. If you change your mind, register again from the event page.</p>
          <Button asChild variant="outline"><Link href="/">Go to CF Meet</Link></Button>
        </div>
      ) : (
        <form className="space-y-5" onSubmit={submit}>
          <div className="space-y-2 text-center">
            <CalendarX2 className="mx-auto size-12 text-primary" />
            <h1 className="text-2xl font-bold">Cancel your registration</h1>
            <p className="text-sm text-muted-foreground">Can&apos;t make it? Cancel with the admission link you got when you registered, so the organizer knows and your place is freed.</p>
          </div>
          <Field label="Admission link" required value={value} onChange={(e) => setTyped(e.target.value)} placeholder="https://…/invite#reg_…" autoComplete="off" spellCheck={false} data-testid="registration-token" />
          {error ? (notFound
            ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">That link isn&apos;t valid any more — it may already have been used, cancelled or have expired.</p>
            : <RequestError error={error} />) : null}
          <Button type="submit" variant="destructive" className="w-full" disabled={busy || !registrationToken(value)} data-testid="registration-cancel">
            {busy ? "Cancelling…" : "Cancel my registration"}
          </Button>
        </form>
      )}
    </main>
  );
}
