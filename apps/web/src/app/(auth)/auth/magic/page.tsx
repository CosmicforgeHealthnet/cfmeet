"use client";

import { Suspense, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { Mail } from "lucide-react";
import { Button } from "@/components/ui/button";
import { authService, MfaRequired } from "@/services/auth.service";
import { getErrorMessage } from "@/utils/error";

/**
 * The one-click sign-in link from the email (/auth/magic?email=…&token=…). It never signs in
 * on page load (email scanners open links); the person presses the button, which POSTs.
 */
function MagicLink() {
  const router = useRouter();
  const params = useSearchParams();
  const email = params.get("email") ?? "";
  const token = params.get("token") ?? "";
  const valid = !!email && /^[0-9a-f]{64}$/i.test(token);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ticket, setTicket] = useState<string | null>(null);
  const [code, setCode] = useState("");

  const go = async () => {
    setBusy(true); setError(null);
    try {
      await authService.verifyLink(email, token);
      router.replace("/dashboard");
    } catch (e) {
      if (e instanceof MfaRequired) { setTicket(e.ticket); setBusy(false); return; }
      setError(getErrorMessage(e, "This link has expired or was already used. Ask for a new code."));
      setBusy(false);
    }
  };
  const finishMfa = async () => {
    if (!ticket || !code.trim()) return;
    setBusy(true); setError(null);
    try { await authService.verifyMfa(ticket, code.trim()); router.replace("/dashboard"); }
    catch (e) { setCode(""); setError(getErrorMessage(e, "That code isn't right. Try again.")); setBusy(false); }
  };

  return (
    <div>
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Mail className="size-6" /></span>
      <h1 className="mt-4 text-3xl font-bold tracking-tight text-foreground">Sign in to CF Meet</h1>
      {valid ? (
        <>
          <p className="mt-2 text-base text-muted-foreground">Continue as <span className="font-semibold text-foreground">{email}</span>.</p>
          {error ? <p role="alert" className="mt-5 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}
          {ticket ? (
            <form className="mt-6 space-y-3" onSubmit={(e) => { e.preventDefault(); void finishMfa(); }}>
              <p className="text-sm text-muted-foreground">Enter the code from your authenticator app, or a recovery code.</p>
              <input autoFocus autoComplete="one-time-code" value={code} onChange={(e) => setCode(e.target.value)} placeholder="123456 or xxxxx-xxxxx" aria-label="Authenticator code"
                className="h-12 w-full rounded-xl border border-transparent bg-muted/70 px-4 text-base outline-none focus:border-primary focus:bg-background focus:ring-2 focus:ring-primary/20" />
              <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none" loading={busy}>Sign in</Button>
            </form>
          ) : <Button type="button" className="mt-6 h-12 w-full rounded-xl text-base shadow-none" loading={busy} onClick={() => void go()}>Continue</Button>}
        </>
      ) : (
        <p className="mt-2 text-base text-muted-foreground">This link isn&apos;t complete. Open it again from the email, or sign in with a code.</p>
      )}
      <button type="button" onClick={() => router.push("/login")} className="mt-4 w-full text-sm font-semibold text-muted-foreground hover:text-foreground">Sign in with a code instead</button>
    </div>
  );
}

export default function MagicLinkPage() {
  return <Suspense><MagicLink /></Suspense>;
}
