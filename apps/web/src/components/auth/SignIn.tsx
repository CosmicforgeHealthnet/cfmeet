"use client";

import { useEffect, useState, useSyncExternalStore } from "react";
import Image from "next/image";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ArrowLeft, Mail, ShieldCheck } from "lucide-react";
import AuthInput from "@/components/auth/AuthInput";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { InputOTP, InputOTPGroup, InputOTPSlot } from "@/components/ui/input-otp";
import { authService, MfaRequired } from "@/services/auth.service";
import { accountService } from "@/services/account.service";
import { accountSession } from "@/lib/auth/session";
import { signInNotice } from "@/lib/account/account-core";
import { safeReturnPath } from "@/lib/organizations/organization-core";
import { getErrorMessage } from "@/utils/error";

const noSubscription = () => () => {};
const returnPath = () =>
  (typeof window !== "undefined" && safeReturnPath(new URLSearchParams(window.location.search).get("next"))) || "/dashboard";
const RESEND_SECONDS = 60;
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

type Step = "email" | "code" | "mfa" | "username";

/**
 * Passwordless sign-in and sign-up (one flow): email → 6-digit code (or the link in the email)
 * → for a brand-new account, pick a username. Google sign-in is unchanged.
 */
export default function SignIn({ mode = "signin" }: { mode?: "signin" | "signup" }) {
  const router = useRouter();
  const [step, setStep] = useState<Step>("email");
  const [email, setEmail] = useState("");
  const [phoneMode, setPhoneMode] = useState(false);
  const [phoneChallenge, setPhoneChallenge] = useState("");
  const [code, setCode] = useState("");
  const [username, setUsername] = useState("");
  const [remember, setRemember] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [cooldown, setCooldown] = useState(0);
  const [ticket, setTicket] = useState("");
  const [mfaCode, setMfaCode] = useState("");
  const search = useSyncExternalStore(noSubscription, () => window.location.search, () => "");
  const notice = signInNotice(new URLSearchParams(search).get("reason"));

  useEffect(() => { if (accountSession.hasSession() && step === "email") router.push(returnPath()); }, [router, step]);
  useEffect(() => {
    if (cooldown <= 0) return;
    const t = setTimeout(() => setCooldown((c) => c - 1), 1000);
    return () => clearTimeout(t);
  }, [cooldown]);

  const sendCode = async () => {
    const address = email.trim();
    if (phoneMode ? !/^\+[1-9]\d{7,14}$/.test(address) : !EMAIL_RE.test(address)) { setError(phoneMode ? "Enter your linked number with its country code, for example +393331234567." : "Enter a valid email address."); return; }
    setBusy(true); setError(null);
    try {
      if (phoneMode) setPhoneChallenge((await authService.requestPhoneCode(address)).challengeId);
      else await authService.requestCode(address);
      setEmail(address); setCode(""); setStep("code"); setCooldown(RESEND_SECONDS);
    } catch (e) { setError(getErrorMessage(e, "We couldn't send the code. Try again.")); }
    finally { setBusy(false); }
  };

  const verify = async (value = code) => {
    if (!/^\d{6}$/.test(value)) return;
    setBusy(true); setError(null);
    try {
      const res = phoneMode
        ? await authService.verifyPhoneCode(phoneChallenge, value, remember)
        : await authService.verifyCode(email, value, remember);
      if (res.is_new_account) { setStep("username"); return; }
      router.push(returnPath());
    } catch (e) {
      setCode("");
      if (e instanceof MfaRequired) { setTicket(e.ticket); setStep("mfa"); return; }
      const status = (e as { response?: { status?: number } }).response?.status;
      setError(status === 429 ? "Too many tries. Wait 15 minutes, then ask for a new code." : getErrorMessage(e, "That code isn't right or has expired."));
    } finally { setBusy(false); }
  };

  const saveUsername = async () => {
    const name = username.trim();
    if (!name) { router.push(returnPath()); return; }
    setBusy(true); setError(null);
    try { await accountService.updateProfile({ username: name }); router.push(returnPath()); }
    catch (e) { setError(getErrorMessage(e, "That username can't be used. Try another.")); }
    finally { setBusy(false); }
  };

  const verifyMfa = async () => {
    const value = mfaCode.trim();
    if (!value) return;
    setBusy(true); setError(null);
    try {
      const res = await authService.verifyMfa(ticket, value, remember);
      if (res.is_new_account) { setStep("username"); return; }
      router.push(returnPath());
    } catch (e) {
      setMfaCode("");
      const status = (e as { response?: { status?: number } }).response?.status;
      if (status === 429) { setStep("email"); setError("Too many tries. Sign in again."); return; }
      setError(getErrorMessage(e, "That code isn't right. Try again."));
    } finally { setBusy(false); }
  };

  if (step === "mfa") {
    return (
      <div>
        <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><ShieldCheck className="size-6" /></span>
        <h1 className="mt-4 text-3xl font-bold tracking-tight text-foreground">Two-step sign-in</h1>
        <p className="mt-2 text-base text-muted-foreground">Enter the 6-digit code from your authenticator app. Lost your phone? Use one of your recovery codes.</p>
        {error ? <p role="alert" className="mt-5 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}
        <form className="mt-6 space-y-5" onSubmit={(e) => { e.preventDefault(); void verifyMfa(); }} noValidate>
          <AuthInput label="Code" autoComplete="one-time-code" placeholder="123456 or xxxxx-xxxxx" value={mfaCode}
            onChange={(e) => { setMfaCode(e.target.value); setError(null); }} />
          <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none" loading={busy}>Sign in</Button>
        </form>
      </div>
    );
  }

  if (step === "username") {
    return (
      <div>
        <h1 className="text-3xl font-bold tracking-tight text-foreground">Welcome to CF Meet</h1>
        <p className="mt-2 text-base text-muted-foreground">Pick a username. People can find you by it.</p>
        {error ? <p role="alert" className="mt-5 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}
        <form className="mt-6 space-y-5" onSubmit={(e) => { e.preventDefault(); void saveUsername(); }} noValidate>
          <AuthInput label="Username" autoComplete="username" placeholder="e.g. ada" value={username}
            onChange={(e) => { setUsername(e.target.value.replace(/\s/g, "")); setError(null); }} />
          <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none" loading={busy}>Continue</Button>
          <button type="button" onClick={() => router.push(returnPath())} className="w-full text-sm font-semibold text-muted-foreground hover:text-foreground">Skip for now</button>
        </form>
      </div>
    );
  }

  if (step === "code") {
    return (
      <div>
        <button type="button" onClick={() => { setStep("email"); setError(null); }} className="-ml-2 mb-4 inline-flex h-9 items-center gap-1.5 rounded-xl px-2 text-sm font-semibold text-muted-foreground hover:bg-muted hover:text-foreground">
          <ArrowLeft className="size-4" />{phoneMode ? "Use a different number" : "Use a different email"}
        </button>
        <span className="flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Mail className="size-6" /></span>
        <h1 className="mt-4 text-3xl font-bold tracking-tight text-foreground">{phoneMode ? "Check your phone" : "Check your email"}</h1>
        <p className="mt-2 text-base text-muted-foreground">
          {phoneMode ? <>If this number is linked to an account, a 6-digit code will arrive at <span className="font-semibold text-foreground">{email}</span>.</> : <>We sent a 6-digit code to <span className="font-semibold text-foreground">{email}</span>. Enter it below, or tap the link in the email.</>}
        </p>
        {error ? <p role="alert" className="mt-5 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}
        <div className="mt-6 flex justify-center">
          <InputOTP maxLength={6} value={code} inputMode="numeric" pattern="^[0-9]*$" autoFocus disabled={busy}
            onChange={(v) => { setCode(v); setError(null); if (v.length === 6) void verify(v); }}>
            <InputOTPGroup className="gap-2">
              {[0, 1, 2, 3, 4, 5].map((i) => <InputOTPSlot key={i} index={i} className="size-12 rounded-xl border border-border text-xl font-semibold sm:size-14" />)}
            </InputOTPGroup>
          </InputOTP>
        </div>
        <Button type="button" className="mt-6 h-12 w-full rounded-xl text-base shadow-none" loading={busy} disabled={code.length !== 6} onClick={() => void verify()}>Continue</Button>
        <p className="mt-5 text-center text-sm text-muted-foreground">
          {phoneMode ? "Didn't get it? " : "Didn't get it? Check spam, or "}
          {cooldown > 0 ? <span>send a new code in {cooldown}s</span>
            : <button type="button" disabled={busy} onClick={() => void sendCode()} className="font-semibold text-primary hover:underline">send a new code</button>}
        </p>
        <p className="mt-2 text-center text-xs text-muted-foreground">The code works for 10 minutes.</p>
      </div>
    );
  }

  return (
    <div>
      <h1 className="text-3xl font-bold tracking-tight text-foreground">{mode === "signup" ? "Create your account" : "Welcome back"}</h1>
      <p className="mt-2 text-base text-muted-foreground">{mode === "signup" ? "No password needed. We'll email you a code." : "Sign in with a code sent to your email. No password needed."}</p>

      <Button type="button" variant="outline" onClick={authService.initiateGoogleOAuth}
        className="mt-6 h-12 w-full rounded-xl border-border bg-background font-semibold text-foreground shadow-none hover:bg-muted">
        <Image src="/GoogleLogo.png" alt="" width={20} height={20} className="size-5" />
        Continue with Google
      </Button>

      <div className="my-5 flex items-center gap-3 text-xs uppercase tracking-wider text-muted-foreground">
        <span className="h-px flex-1 bg-border" />or<span className="h-px flex-1 bg-border" />
      </div>

      {notice && !error ? <div role="status" className="mb-4 rounded-xl bg-muted px-4 py-3 text-sm">{notice}</div> : null}
      {error ? <p role="alert" className="mb-4 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}

      <form className="space-y-5" onSubmit={(e) => { e.preventDefault(); void sendCode(); }} noValidate>
        <AuthInput label={phoneMode ? "Linked phone number" : "Email"} type={phoneMode ? "tel" : "email"} autoComplete={phoneMode ? "tel" : "email"} placeholder={phoneMode ? "+393331234567" : "you@example.com"} value={email}
          onChange={(e) => { setEmail(e.target.value); setError(null); }} />
        <label htmlFor="remember-me" className="flex cursor-pointer items-center gap-2 text-sm text-foreground">
          <Checkbox id="remember-me" checked={remember} onCheckedChange={(c) => setRemember(c === true)} />
          Keep me signed in on this device
        </label>
        <Button type="submit" className="h-12 w-full rounded-xl text-base shadow-none" loading={busy}>{phoneMode ? "Continue with phone" : "Continue with email"}</Button>
      </form>
      {mode === "signin" && process.env.NEXT_PUBLIC_PHONE_ACCOUNTS_ENABLED === "true" ? <button type="button" disabled={busy} className="mt-4 w-full text-sm font-semibold text-primary" onClick={() => { setPhoneMode(!phoneMode); setEmail(""); setError(null); }}>{phoneMode ? "Use email instead" : "Sign in with a linked phone number"}</button> : null}

      <p className="mt-6 text-center text-sm text-muted-foreground">
        {mode === "signup" ? "Already have an account? " : "New to CF Meet? "}
        <Link href={mode === "signup" ? "/login" : "/signup"} className="font-semibold text-primary hover:underline">{mode === "signup" ? "Sign in" : "Create an account"}</Link>
      </p>
      <p className="mt-6 border-t border-border pt-5 text-center text-sm text-muted-foreground">
        Joining a meeting?{" "}
        <Link href="/join" className="font-semibold text-foreground hover:text-primary hover:underline">Join as a guest</Link>
      </p>
    </div>
  );
}
