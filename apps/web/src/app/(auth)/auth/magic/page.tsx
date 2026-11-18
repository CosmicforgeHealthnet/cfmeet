"use client";

import { Suspense, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { Mail } from "lucide-react";
import { Button } from "@/components/ui/button";
import { authService } from "@/services/auth.service";
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

  const go = async () => {
    setBusy(true); setError(null);
    try {
      await authService.verifyLink(email, token);
      router.replace("/dashboard");
    } catch (e) {
      setError(getErrorMessage(e, "This link has expired or was already used. Ask for a new code."));
      setBusy(false);
    }
  };

  return (
    <div>
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Mail className="size-6" /></span>
      <h1 className="mt-4 text-3xl font-bold tracking-tight text-foreground">Sign in to CF Meet</h1>
      {valid ? (
        <>
          <p className="mt-2 text-base text-muted-foreground">Continue as <span className="font-semibold text-foreground">{email}</span>.</p>
          {error ? <p role="alert" className="mt-5 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}
          <Button type="button" className="mt-6 h-12 w-full rounded-xl text-base shadow-none" loading={busy} onClick={() => void go()}>Continue</Button>
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
