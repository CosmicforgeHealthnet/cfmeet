"use client";

import { useEffect, useState } from "react";
import { InputOTP, InputOTPGroup, InputOTPSlot } from "@/components/ui/input-otp";
import { Button } from "@/components/ui/button";
import { getErrorMessage } from "@/utils/error";

/**
 * "We emailed you a code" step: sends a code when shown, takes the 6 digits, resend after 60 s.
 * Used for step-up confirmation (deactivate, change email) and for confirming a new address.
 */
export default function EmailCodeConfirm({ email, send, verify, confirmLabel = "Confirm", danger }: {
  /** Where the code went (shown to the person). */
  email: string;
  send: () => Promise<unknown>;
  verify: (code: string) => Promise<unknown>;
  confirmLabel?: string;
  danger?: boolean;
}) {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [cooldown, setCooldown] = useState(0);
  const [sent, setSent] = useState(false);

  const sendCode = async () => {
    setError(null);
    try { await send(); setSent(true); setCooldown(60); }
    catch (e) { setError(getErrorMessage(e, "We couldn't send the code. Try again.")); }
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => { void sendCode(); }, []);
  useEffect(() => {
    if (cooldown <= 0) return;
    const t = setTimeout(() => setCooldown((c) => c - 1), 1000);
    return () => clearTimeout(t);
  }, [cooldown]);

  const confirm = async (value = code) => {
    if (!/^\d{6}$/.test(value)) return;
    setBusy(true); setError(null);
    try { await verify(value); }
    catch (e) {
      setCode("");
      const status = (e as { response?: { status?: number } }).response?.status;
      setError(status === 429 ? "Too many tries. Wait 15 minutes, then ask for a new code." : getErrorMessage(e, "That code isn't right or has expired."));
    } finally { setBusy(false); }
  };

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{sent ? <>We sent a 6-digit code to <span className="font-semibold text-foreground">{email}</span>.</> : "Sending a code…"}</p>
      <div className="flex justify-center">
        <InputOTP maxLength={6} value={code} inputMode="numeric" pattern="^[0-9]*$" autoFocus disabled={busy}
          onChange={(v) => { setCode(v); setError(null); if (v.length === 6) void confirm(v); }}>
          <InputOTPGroup className="gap-2">
            {[0, 1, 2, 3, 4, 5].map((i) => <InputOTPSlot key={i} index={i} className="size-11 rounded-xl border border-border text-lg font-semibold sm:size-12" />)}
          </InputOTPGroup>
        </InputOTP>
      </div>
      {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
      <Button type="button" variant={danger ? "destructive" : "default"} className="h-11 w-full rounded-xl" loading={busy} disabled={code.length !== 6} onClick={() => void confirm()}>{confirmLabel}</Button>
      <p className="text-center text-sm text-muted-foreground">
        {cooldown > 0 ? `Send a new code in ${cooldown}s` : <button type="button" onClick={() => void sendCode()} className="font-semibold text-primary hover:underline">Send a new code</button>}
      </p>
    </div>
  );
}
