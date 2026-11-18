"use client";

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Copy, ShieldCheck, Smartphone } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import QrCode from "@/components/communications/QrCode";
import EmailCodeConfirm from "./EmailCodeConfirm";
import { accountService } from "@/services/account.service";
import { useAccountProfile } from "@/hooks/useAccount";
import { getErrorMessage } from "@/utils/error";

const KEY = ["me", "mfa"] as const;

/** Settings → Security: two-step sign-in with an authenticator app (Google Authenticator, Authy…). */
export default function TwoStepCard() {
  const client = useQueryClient();
  const status = useQuery({ queryKey: KEY, queryFn: accountService.mfaStatus, retry: false });
  const profile = useAccountProfile();
  const [setupOpen, setSetupOpen] = useState(false);
  const [offOpen, setOffOpen] = useState(false);
  if (status.isError) return null; // not offered here
  const on = !!status.data?.totpEnabled;

  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex gap-3">
          <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><ShieldCheck className="size-5" /></span>
          <div>
            <h2 className="text-base font-semibold">Two-step sign-in</h2>
            <p className="mt-0.5 text-sm text-muted-foreground">
              {on ? `On. After the email code, you also enter a code from your authenticator app. ${status.data?.recoveryCodesRemaining ?? 0} recovery codes left.`
                : "Add a code from an authenticator app (like Google Authenticator) after the email code."}
            </p>
          </div>
        </div>
        {status.data ? (on
          ? <Button variant="outline" className="h-9 rounded-xl" onClick={() => setOffOpen(true)}>Turn off</Button>
          : <Button className="h-9 rounded-xl" onClick={() => setSetupOpen(true)}>Turn on</Button>) : null}
      </div>
      {setupOpen ? <SetupDialog onClose={() => { setSetupOpen(false); void client.invalidateQueries({ queryKey: KEY }); }} /> : null}
      <Dialog open={offOpen} onOpenChange={setOffOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader><DialogTitle>Turn off two-step sign-in</DialogTitle><DialogDescription>Confirm with the code we email you.</DialogDescription></DialogHeader>
          {offOpen && profile.data ? (
            <EmailCodeConfirm email={profile.data.email} danger confirmLabel="Turn off"
              send={() => accountService.requestStepUp("DISABLE_MFA")}
              verify={async (code) => {
                const { stepUpToken } = await accountService.verifyStepUp("DISABLE_MFA", code);
                await accountService.mfaDisable(stepUpToken);
                setOffOpen(false);
                void client.invalidateQueries({ queryKey: KEY });
              }} />
          ) : null}
        </DialogContent>
      </Dialog>
    </section>
  );
}

function SetupDialog({ onClose }: { onClose: () => void }) {
  const setup = useQuery({ queryKey: ["me", "mfa", "setup"], queryFn: accountService.mfaSetup, retry: false, staleTime: Infinity, gcTime: 0 });
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [copied, setCopied] = useState(false);
  const confirm = async () => {
    if (!/^\d{6}$/.test(code.trim())) { setError("Enter the 6-digit code from the app."); return; }
    setBusy(true); setError(null);
    try { setCodes((await accountService.mfaConfirm(code.trim())).recoveryCodes); }
    catch (e) { setCode(""); setError(getErrorMessage(e, "That code isn't right. Check the time on your phone and try again.")); }
    finally { setBusy(false); }
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{codes ? "Save your recovery codes" : "Turn on two-step sign-in"}</DialogTitle>
          <DialogDescription>{codes ? "If you lose your phone, each code signs you in once. Keep them somewhere safe; they won't be shown again." : "Scan this with your authenticator app, then enter the 6-digit code it shows."}</DialogDescription>
        </DialogHeader>
        {codes ? (
          <div className="space-y-3">
            <ul className="grid grid-cols-2 gap-2 rounded-xl bg-muted/60 p-3 font-mono text-sm">{codes.map((c) => <li key={c}>{c}</li>)}</ul>
            <Button variant="outline" className="h-10 w-full rounded-xl" onClick={() => { void navigator.clipboard?.writeText(codes.join("\n")); setCopied(true); }}>
              {copied ? <Check className="size-4" /> : <Copy className="size-4" />}{copied ? "Copied" : "Copy codes"}
            </Button>
            <Button className="h-11 w-full rounded-xl" onClick={onClose}>Done</Button>
          </div>
        ) : setup.isError ? (
          <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{getErrorMessage(setup.error, "Two-step sign-in can't be set up right now.")}</p>
        ) : !setup.data ? (
          <div className="h-48 animate-pulse rounded-xl bg-muted" />
        ) : (
          <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); void confirm(); }}>
            <div className="flex justify-center rounded-xl bg-white p-3"><QrCode value={setup.data.otpauthUri} label="Authenticator app setup code" /></div>
            <p className="flex items-start gap-2 text-xs text-muted-foreground"><Smartphone className="mt-0.5 size-3.5 shrink-0" />Can&apos;t scan? Enter this key in the app: <span className="break-all font-mono text-foreground">{setup.data.secret}</span></p>
            <Input inputMode="numeric" autoComplete="one-time-code" placeholder="6-digit code" value={code} onChange={(e) => { setCode(e.target.value.replace(/\D/g, "").slice(0, 6)); setError(null); }} />
            {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
            <Button type="submit" className="h-11 w-full rounded-xl" loading={busy}>Turn on</Button>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
