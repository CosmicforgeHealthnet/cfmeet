"use client";

import { Suspense, useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { authService } from "@/services/auth.service";

/**
 * "Undo email change" from the link sent to the previous address (valid 7 days). Restores the
 * old address and ends every session; nothing happens until the person presses the button.
 */
function UndoEmailChange() {
  const token = useSearchParams().get("token") ?? "";
  const valid = /^[0-9a-f]{64}$/i.test(token);
  const [state, setState] = useState<"idle" | "busy" | "done" | "invalid" | "taken" | "error">(valid ? "idle" : "invalid");

  const undo = async () => {
    setState("busy");
    try { await authService.revertEmailChange(token); setState("done"); }
    catch (e) {
      const status = (e as { response?: { status?: number } }).response?.status;
      setState(status === 409 ? "taken" : status === 400 ? "invalid" : "error");
    }
  };

  return (
    <div>
      <span className="flex size-12 items-center justify-center rounded-2xl bg-amber-500/10 text-amber-600"><ShieldAlert className="size-6" /></span>
      <h1 className="mt-4 text-3xl font-bold tracking-tight text-foreground">Undo email change</h1>
      {state === "done" ? (
        <>
          <p className="mt-2 text-base text-muted-foreground">Your previous email address is back, and everyone was signed out. Sign in with your previous address.</p>
          <Button asChild className="mt-6 h-12 w-full rounded-xl text-base shadow-none"><Link href="/login">Sign in</Link></Button>
        </>
      ) : state === "invalid" ? (
        <p className="mt-2 text-base text-muted-foreground">This link is invalid, already used or expired (links work for 7 days).</p>
      ) : state === "taken" ? (
        <p className="mt-2 text-base text-muted-foreground">Your previous address now belongs to another account, so it can&apos;t be restored. Contact CF Meet support.</p>
      ) : (
        <>
          <p className="mt-2 text-base text-muted-foreground">If you didn&apos;t change your email, undo it here. Your previous address comes back and every device is signed out.</p>
          {state === "error" ? <p role="alert" className="mt-5 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">That didn&apos;t work. Try again.</p> : null}
          <Button type="button" className="mt-6 h-12 w-full rounded-xl text-base shadow-none" loading={state === "busy"} onClick={() => void undo()}>Undo the change</Button>
        </>
      )}
    </div>
  );
}

export default function UndoEmailChangePage() {
  return <Suspense><UndoEmailChange /></Suspense>;
}
