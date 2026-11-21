"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { accountService } from "@/services/account.service";
import { authService } from "@/services/auth.service";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import EmailCodeConfirm from "./EmailCodeConfirm";
import { getErrorMessage } from "@/utils/error";

export default function PhoneSettings({ email }: { email: string }) {
  const router = useRouter();
  const query = useQuery({ queryKey: ["account-phone"], queryFn: accountService.phone, retry: false });
  const client = useQueryClient();
  const [action, setAction] = useState<"add" | "remove" | null>(null);
  const [stage, setStage] = useState<"email" | "sms">("email");
  const [phone, setPhone] = useState("");
  const [challenge, setChallenge] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = () => { void client.invalidateQueries({ queryKey: ["account-phone"] }); };
  const saveCode = async () => {
    setBusy(true); setError(null);
    try { await accountService.confirmPhone(challenge, code); setAction(null); refresh(); }
    catch (cause) { setError(getErrorMessage(cause, "The phone code could not be verified")); }
    finally { setBusy(false); }
  };
  const open = (next: "add" | "remove") => { setStage("email"); setCode(""); setError(null); setAction(next); };
  return (
    <section className="space-y-3 rounded-2xl border border-border p-5" aria-label="Phone number">
      <h2 className="font-semibold">Phone number</h2>
      {query.isLoading ? <p className="text-sm text-muted-foreground">Loading…</p> : query.isError ? <p role="alert" className="text-sm text-muted-foreground">Phone settings are unavailable right now.</p> : query.data?.status === "PROVIDER_BLOCKED" ? <p className="text-sm text-muted-foreground">Phone verification is not available yet. Your email sign-in stays available.</p> : <>
        <p className="text-sm">{query.data?.phoneNumber ?? "No phone number linked"}</p>
        <p className="text-sm text-muted-foreground">A verified number gives you another way to sign in. Your authenticator still applies.</p>
        {query.data?.phoneNumber ? <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={query.data.discoverable} disabled={busy} onChange={async (event) => {
          setBusy(true); setError(null);
          try { await accountService.phonePrivacy(event.target.checked); refresh(); }
          catch (cause) { setError(getErrorMessage(cause, "Your phone privacy could not be saved")); }
          finally { setBusy(false); }
        }} />Allow people to find me by this number</label> : null}
        <div className="flex flex-wrap gap-2">
          <input aria-label="International phone number" type="tel" autoComplete="tel" value={phone} onChange={(event) => setPhone(event.target.value)} placeholder="+393331234567" className="h-10 rounded-xl border px-3" />
          <button type="button" onClick={() => open("add")} disabled={!/^\+[1-9]\d{7,14}$/.test(phone)} className="h-10 rounded-xl bg-primary px-4 text-primary-foreground disabled:opacity-50">{query.data?.phoneNumber ? "Change number" : "Add number"}</button>
          {query.data?.phoneNumber ? <button type="button" onClick={() => open("remove")} className="h-10 rounded-xl px-4 text-destructive">Remove</button> : null}
        </div>
      </>}
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      <Dialog open={!!action} onOpenChange={(value) => { if (!value && !busy) setAction(null); }}>
        <DialogContent>
          <DialogHeader><DialogTitle>{action === "remove" ? "Remove your phone number" : "Verify your phone number"}</DialogTitle><DialogDescription>{action === "remove" ? "Confirm by email. Removing your number signs your devices out; sign in again with email." : "Confirm your account by email, then enter the code sent to your phone."}</DialogDescription></DialogHeader>
          {stage === "email" ? <EmailCodeConfirm email={email} send={() => accountService.requestStepUp("MANAGE_PHONE")} verify={async (value) => {
            const token = await accountService.verifyStepUp("MANAGE_PHONE", value);
            if (action === "remove") { await accountService.removePhone(token.stepUpToken); await authService.forgetLocalSession(); router.replace("/login"); return; }
            const sent = await accountService.requestPhone(phone, token.stepUpToken);
            setChallenge(sent.challengeId); setStage("sms");
          }} /> : <form className="space-y-3" onSubmit={(event) => { event.preventDefault(); void saveCode(); }}>
            <label className="block text-sm">Code sent to {phone}<input className="mt-1 h-11 w-full rounded-xl border px-3" autoComplete="one-time-code" inputMode="numeric" maxLength={6} value={code} onChange={(event) => setCode(event.target.value.replace(/\D/g, ""))} /></label>
            <button disabled={busy || code.length !== 6} className="h-10 rounded-xl bg-primary px-4 text-primary-foreground disabled:opacity-50">Verify phone</button>
          </form>}
        </DialogContent>
      </Dialog>
    </section>
  );
}
