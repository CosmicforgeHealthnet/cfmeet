"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { NiceSelect } from "@/components/ui/nice-select";
import { TimeZoneField } from "@/components/meetings/ScheduleFields";
import { viewerTimeZone } from "@/lib/meeting-time";
import { BadgeCheck, Check, Copy, Hash, Mail } from "lucide-react";
import { useHubProfile } from "@/hooks/v2/useCommunications";
import { communicationsService as hub } from "@/services/v2/communications.service";
import EmailCodeConfirm from "./EmailCodeConfirm";
import { getErrorMessage } from "@/utils/error";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { useAccountProfile, useUpdateAccountProfile } from "@/hooks/useAccount";
import { accountService } from "@/services/account.service";
import { AVATAR_ERROR_MESSAGES, prepareAvatarImage } from "@/lib/account/avatar-image";
import { authService } from "@/services/auth.service";
import {
  accountStatusLabel,
  apiErrorOf,
  deactivationOutcome,
  normalizeAccountStatus,
  profileForm,
  profilePatch,
  profileProblems,
  type ProfileForm,
} from "@/lib/account/account-core";
import type { AccountProfile } from "@/types/generated/meeting-service";

/** Settings → Profile: `GET/PATCH /api/v2/me/profile` (Section 9) and self-deactivation (ACC-026). */
export default function AccountProfileSettings() {
  const profile = useAccountProfile();

  if (profile.isLoading) {
    return (
      <div className="space-y-4" aria-busy="true"><div className="h-28 animate-pulse rounded-2xl bg-muted" /><div className="h-80 animate-pulse rounded-2xl bg-muted" /></div>
    );
  }
  if (profile.isError || !profile.data) {
    return (
      <div role="alert" className="flex items-center gap-3 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">
        <span className="flex-1">Your profile couldn&apos;t load.</span>
        <button type="button" className="font-semibold hover:underline" onClick={() => void profile.refetch()}>Try again</button>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Remount on a new server version so the form starts from the saved values. */}
      <ProfileEditor key={`${profile.data.userId}:${profile.data.profileUpdatedAt ?? ""}:${profile.data.username}`} profile={profile.data} />
      <YourData />
      <DangerZone profile={profile.data} />
    </div>
  );
}

function ProfileEditor({ profile }: { profile: AccountProfile }) {
  const update = useUpdateAccountProfile();
  const [form, setForm] = useState<ProfileForm>(() => profileForm(profile));
  const [saved, setSaved] = useState(false);
  const problems = profileProblems(form);
  const patch = profilePatch(profile, form);
  // The About line contacts see lives in the Communications Hub (PATCH /api/v2/communications/me).
  const hubProfile = useHubProfile();
  const [about, setAbout] = useState<string | null>(null);
  const [aboutError, setAboutError] = useState<string | null>(null);
  const savedAbout = hubProfile.data?.about ?? "";
  const aboutValue = about ?? savedAbout;
  const aboutChanged = about !== null && about.trim() !== savedAbout;
  const aboutProblem = [...aboutValue].length > 140 ? "Up to 140 characters." : /\p{Cc}/u.test(aboutValue) ? "Use a single line of text." : null;
  const changed = Object.keys(patch).length > 0 || aboutChanged;
  const set = (field: keyof ProfileForm) => (e: React.ChangeEvent<HTMLInputElement>) => {
    setForm((f) => ({ ...f, [field]: e.target.value }));
    setSaved(false);
  };
  const error = update.isError ? apiErrorOf(update.error) : null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!changed || Object.keys(problems).length || aboutProblem) return;
    try {
      if (Object.keys(patch).length > 0) await update.mutateAsync(patch);
      if (aboutChanged && hubProfile.data) {
        setAboutError(null);
        try {
          // The Hub profile version also moves when the account name changes: read it fresh.
          const current = await hub.me();
          await hub.updateMe(current.version, about!.trim() || null);
          await hubProfile.refetch();
          setAbout(null);
        } catch (e) {
          setAboutError(getErrorMessage(e, "Your About line couldn't be saved. Try again."));
          return;
        }
      }
      setSaved(true);
    } catch {
      /* shown below */
    }
  };

  const field = (id: keyof ProfileForm, label: string, hint?: string, placeholder?: string) => (
    <div className="space-y-1.5">
      <Label htmlFor={`profile-${id}`}>{label}</Label>
      <Input id={`profile-${id}`} value={form[id]} onChange={set(id)} placeholder={placeholder} aria-invalid={!!problems[id]} className="h-11 rounded-xl" />
      {problems[id] ? <p className="text-xs text-destructive">{problems[id]}</p> : hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
  const name = form.displayName.trim() || profile.effectiveDisplayName || form.username;
  const avatar = form.avatarUrl.trim().startsWith("https://") && !problems.avatarUrl ? form.avatarUrl.trim() : null;
  const deviceZone = viewerTimeZone();

  return (
    <form onSubmit={(e) => void submit(e)} className="space-y-6">
      {/* Identity card */}
      <section className="flex flex-wrap items-center gap-4 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <AvatarUpload avatar={avatar} initials={name.split(/\s+/).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("")}
          onUploaded={(url) => setForm((f) => ({ ...f, avatarUrl: url }))} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-lg font-semibold">{name}</p>
          <p className="flex items-center gap-1.5 truncate text-sm text-muted-foreground"><Mail className="size-3.5 shrink-0" />{profile.email}{profile.emailVerified ? <BadgeCheck className="size-4 shrink-0 text-primary" aria-label="Verified" /> : null}</p>
          <MyCfMeetId />
          <p className="mt-1.5 flex flex-wrap gap-1.5 text-xs">
            <span className="rounded-full bg-muted px-2 py-0.5 font-medium text-muted-foreground">Signs in with {profile.authType.toLowerCase() === "google" ? "Google" : "an email code"}</span>
            <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 font-medium text-emerald-700 dark:text-emerald-400">{accountStatusLabel(normalizeAccountStatus(profile.status))}</span>
            {!profile.emailVerified && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 font-medium text-amber-700 dark:text-amber-300">Email not verified</span>}
          </p>
        </div>
        <ChangeEmail current={profile.email} />
      </section>

      {error && (
        <div role="alert" className="rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {error.code === "USERNAME_TAKEN" ? "That username is already taken. Try another." : error.message ?? "Your profile couldn't be saved."}
        </div>
      )}

      <section className="space-y-5 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <div>
          <h2 className="text-base font-semibold">About you</h2>
          <p className="mt-0.5 text-sm text-muted-foreground">People see your display name in meetings and chats.</p>
        </div>
        <div className="grid gap-5 sm:grid-cols-2">
          {field("displayName", "Display name", "Up to 64 characters.", profile.username)}
          {field("username", "Username", "3–50 letters, numbers or underscores.")}
          {hubProfile.data ? (
            <div className="space-y-1.5 sm:col-span-2">
              <Label htmlFor="profile-about">About</Label>
              <Input id="profile-about" value={aboutValue} onChange={(e) => { setAbout(e.target.value); setSaved(false); setAboutError(null); }} placeholder="Available" aria-invalid={!!aboutProblem} className="h-11 rounded-xl" />
              {aboutProblem || aboutError ? <p className="text-xs text-destructive">{aboutProblem ?? aboutError}</p> : <p className="text-xs text-muted-foreground">Your contacts see this. Up to 140 characters.</p>}
            </div>
          ) : null}
        </div>
      </section>

      <section className="space-y-5 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <div>
          <h2 className="text-base font-semibold">Region</h2>
          <p className="mt-0.5 text-sm text-muted-foreground">Used for meeting times and the app&apos;s language.</p>
        </div>
        <div className="grid gap-5 sm:grid-cols-2">
          <div className="space-y-1.5">
            <TimeZoneField id="profile-timezone" label="Time zone" value={form.timezone || deviceZone} onChange={(zone) => { setForm((f) => ({ ...f, timezone: zone })); setSaved(false); }} />
            {form.timezone ? (
              <button type="button" onClick={() => { setForm((f) => ({ ...f, timezone: "" })); setSaved(false); }} className="text-xs font-semibold text-primary hover:underline">Use this device&apos;s time zone</button>
            ) : <p className="text-xs text-muted-foreground">Following this device ({deviceZone.replaceAll("_", " ")}).</p>}
            {problems.timezone ? <p className="text-xs text-destructive">{problems.timezone}</p> : null}
          </div>
          <div className="space-y-1.5">
            <NiceSelect label="Language" value={form.locale} onChange={(v) => { setForm((f) => ({ ...f, locale: v })); setSaved(false); }}
              options={[{ value: "", label: "Same as this device" }, ...LANGUAGES.filter((l) => l.value !== "" ), ...(form.locale && !LANGUAGES.some((l) => l.value === form.locale) ? [{ value: form.locale, label: form.locale }] : [])]} />
            {problems.locale ? <p className="text-xs text-destructive">{problems.locale}</p> : null}
          </div>
        </div>
      </section>

      {/* Save bar: only when there is something to save */}
      <div className={`sticky bottom-20 z-10 flex items-center justify-between gap-3 rounded-2xl border border-border bg-card/95 px-4 py-3 shadow-lg backdrop-blur transition-all md:bottom-4 ${changed || saved ? "translate-y-0 opacity-100" : "pointer-events-none translate-y-2 opacity-0"}`}>
        <p className="text-sm text-muted-foreground">{saved && !changed ? <span className="flex items-center gap-1.5 font-medium text-emerald-600"><Check className="size-4" />Saved</span> : "You have unsaved changes."}</p>
        {changed && (
          <div className="flex gap-2">
            <button type="button" onClick={() => { setForm(profileForm(profile)); setAbout(null); setAboutError(null); }} className="h-9 rounded-full px-4 text-sm font-semibold hover:bg-muted">Discard</button>
            <Button type="submit" className="rounded-full" loading={update.isPending} disabled={update.isPending || Object.keys(problems).length > 0 || !!aboutProblem}>Save changes</Button>
          </div>
        )}
      </div>
    </form>
  );
}

const LANGUAGES = [
  { value: "en", label: "English" },
  { value: "en-GB", label: "English (UK)" },
  { value: "fr", label: "Français" },
  { value: "es", label: "Español" },
  { value: "pt", label: "Português" },
  { value: "de", label: "Deutsch" },
  { value: "ar", label: "العربية" },
  { value: "yo", label: "Yorùbá" },
  { value: "ha", label: "Hausa" },
  { value: "ig", label: "Igbo" },
  { value: "sw", label: "Kiswahili" },
];

/** Your CF Meet ID: what people type to add you as a contact. */
function MyCfMeetId() {
  const profile = useHubProfile();
  const [copied, setCopied] = useState(false);
  const id = profile.data?.cfMeetId;
  if (!id) return null;
  return (
    <button type="button" title="Copy your CF Meet ID" data-testid="profile-cfmeet-id"
      onClick={() => void navigator.clipboard?.writeText(id).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); })}
      className="mt-1 inline-flex max-w-full items-center gap-1.5 rounded-lg text-sm text-muted-foreground hover:text-foreground">
      <Hash className="size-3.5 shrink-0" /><span className="truncate font-mono">{id}</span>
      {copied ? <Check className="size-3.5 shrink-0 text-primary" /> : <Copy className="size-3.5 shrink-0" />}
    </button>
  );
}

/** Tap the photo to upload a new one; large photos are shrunk to fit the 512 KB limit. */
function AvatarUpload({ avatar, initials, onUploaded }: { avatar: string | null; initials: string; onUploaded: (url: string) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pick = async (file: File | undefined) => {
    if (!file) return;
    setBusy(true); setError(null);
    try {
      // Shrink phone photos in the browser so they fit the server's 512 KB limit.
      const prepared = await prepareAvatarImage(file);
      if (!prepared.ok) { setError(AVATAR_ERROR_MESSAGES[prepared.error]); return; }
      const p = await accountService.uploadAvatar(prepared.file);
      if (p.avatarUrl) onUploaded(p.avatarUrl);
    }
    catch (e) { setError(getErrorMessage(e, "The photo couldn't be uploaded. Try again.")); }
    finally { setBusy(false); }
  };
  return (
    <div className="flex flex-col items-center gap-1">
      <label className="group relative size-16 shrink-0 cursor-pointer overflow-hidden rounded-full" title="Change photo">
        {avatar ? (
          // eslint-disable-next-line @next/next/no-img-element
          <img src={avatar} alt="" className="size-16 object-cover" />
        ) : (
          <span className="flex size-16 items-center justify-center bg-primary/10 text-xl font-semibold text-primary">{initials}</span>
        )}
        <span className="absolute inset-0 flex items-center justify-center bg-black/45 text-[11px] font-semibold text-white opacity-0 transition-opacity group-hover:opacity-100">{busy ? "Uploading…" : "Change"}</span>
        <input type="file" accept="image/*" className="sr-only" disabled={busy} onChange={(e) => { void pick(e.target.files?.[0]); e.target.value = ""; }} />
      </label>
      {error ? <p role="alert" className="max-w-[10rem] text-center text-[11px] text-destructive">{error}</p> : null}
    </div>
  );
}

/** Download everything CF Meet keeps about you (one JSON file). */
function YourData() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const download = async () => {
    setBusy(true); setError(null);
    try {
      const data = await accountService.dataExport();
      const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }));
      const a = document.createElement("a");
      a.href = url; a.download = `cfmeet-my-data-${new Date().toISOString().slice(0, 10)}.json`; a.click();
      URL.revokeObjectURL(url);
    } catch (e) { setError(getErrorMessage(e, "Your data couldn't be prepared. Try again.")); }
    finally { setBusy(false); }
  };
  return (
    <div className="flex flex-col justify-between gap-3 rounded-2xl border border-border bg-card p-5 sm:flex-row sm:items-center sm:p-6">
      <div className="space-y-1">
        <p className="font-semibold">Your data</p>
        <p className="text-sm text-muted-foreground">Download a copy of what CF Meet keeps about you.</p>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      </div>
      <Button variant="outline" className="w-full rounded-xl sm:w-auto" loading={busy} onClick={() => void download()}>Download my data</Button>
    </div>
  );
}

function DangerZone({ profile }: { profile: AccountProfile }) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  return (
    <div className="rounded-2xl border border-destructive/30 bg-card p-5 sm:p-6">
      <div className="flex flex-col justify-between gap-4 sm:flex-row sm:items-center">
        <div className="space-y-1">
          <p className="font-semibold text-destructive">Deactivate account</p>
          <p className="text-sm text-muted-foreground">Signs you out everywhere and stops you signing in. Only CF Meet support can turn it back on.</p>
        </div>
        <Dialog open={open} onOpenChange={setOpen}>
          <DialogTrigger asChild>
            <Button variant="outline" className="w-full rounded-xl border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive sm:w-auto">
              Deactivate
            </Button>
          </DialogTrigger>
          <DialogContent className="sm:max-w-md">
            <DialogHeader>
              <DialogTitle>Deactivate account</DialogTitle>
              <DialogDescription>You&apos;ll be signed out on every device and can&apos;t sign in until CF Meet support turns the account back on. To confirm, enter the code we email you.</DialogDescription>
            </DialogHeader>
            {open ? (
              <EmailCodeConfirm email={profile.email} danger confirmLabel="Deactivate account"
                send={() => accountService.requestStepUp("DEACTIVATE_ACCOUNT")}
                verify={async (code) => {
                  const { stepUpToken } = await accountService.verifyStepUp("DEACTIVATE_ACCOUNT", code);
                  await accountService.deactivate(stepUpToken);
                  await authService.forgetLocalSession();
                  router.replace("/login?reason=deactivated");
                }} />
            ) : null}
          </DialogContent>
        </Dialog>
      </div>
      <div className="mt-5 flex flex-col justify-between gap-4 border-t border-destructive/20 pt-5 sm:flex-row sm:items-center">
        <div className="space-y-1">
          <p className="font-semibold text-destructive">Delete account</p>
          <p className="text-sm text-muted-foreground">Deletes your account and data for good. This can&apos;t be undone.</p>
        </div>
        <Dialog open={deleting} onOpenChange={setDeleting}>
          <DialogTrigger asChild>
            <Button variant="destructive" className="w-full rounded-xl sm:w-auto">Delete account</Button>
          </DialogTrigger>
          <DialogContent className="sm:max-w-md">
            <DialogHeader>
              <DialogTitle>Delete your account for good?</DialogTitle>
              <DialogDescription>Your account, contacts, chats on CF Meet and settings are deleted and can&apos;t be brought back. Download your data first if you want a copy. To confirm, enter the code we email you.</DialogDescription>
            </DialogHeader>
            {deleting ? (
              <EmailCodeConfirm email={profile.email} danger confirmLabel="Delete my account"
                send={() => accountService.requestStepUp("DELETE_ACCOUNT")}
                verify={async (code) => {
                  const { stepUpToken } = await accountService.verifyStepUp("DELETE_ACCOUNT", code);
                  await accountService.deleteAccount(stepUpToken);
                  await authService.forgetLocalSession();
                  router.replace("/login?reason=deleted");
                }} />
            ) : null}
          </DialogContent>
        </Dialog>
      </div>
    </div>
  );
}

/** Change email: confirm with a code to the current address, then a code to the new one. */
function ChangeEmail({ current }: { current: string }) {
  const [open, setOpen] = useState(false);
  const [step, setStep] = useState<"confirm" | "address" | "verify" | "done">("confirm");
  const [token, setToken] = useState("");
  const [address, setAddress] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const reset = (v: boolean) => { setOpen(v); if (!v) { setStep("confirm"); setToken(""); setAddress(""); setError(null); } };
  const submitAddress = async () => {
    const next = address.trim();
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(next)) { setError("Enter a valid email address."); return; }
    setBusy(true); setError(null);
    try { await accountService.requestEmailChange(next, token); setAddress(next); setStep("verify"); }
    catch (e) {
      const status = (e as { response?: { status?: number } }).response?.status;
      setError(status === 429 ? "You can change your email once every 7 days." : getErrorMessage(e, "That didn't work. Try again."));
    } finally { setBusy(false); }
  };
  return (
    <Dialog open={open} onOpenChange={reset}>
      <DialogTrigger asChild>
        <Button variant="outline" className="h-9 shrink-0 rounded-xl px-3.5 text-sm">Change email</Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Change email</DialogTitle>
          <DialogDescription>
            {step === "confirm" ? "First, confirm it's you with a code sent to your current email."
              : step === "address" ? "Enter your new email. We'll send a code there."
              : step === "verify" ? "Enter the code we sent to your new email. Your other devices will be signed out."
              : "Your email was changed."}
          </DialogDescription>
        </DialogHeader>
        {step === "confirm" && open ? (
          <EmailCodeConfirm email={current} confirmLabel="Continue"
            send={() => accountService.requestStepUp("CHANGE_EMAIL")}
            verify={async (code) => { const r = await accountService.verifyStepUp("CHANGE_EMAIL", code); setToken(r.stepUpToken); setStep("address"); }} />
        ) : step === "address" ? (
          <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); void submitAddress(); }}>
            <Input type="email" autoFocus autoComplete="email" placeholder="new@example.com" value={address} onChange={(e) => { setAddress(e.target.value); setError(null); }} />
            {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
            <Button type="submit" className="h-11 w-full rounded-xl" loading={busy}>Send code</Button>
          </form>
        ) : step === "verify" ? (
          <EmailCodeConfirm email={address} confirmLabel="Change email"
            send={async () => undefined}
            verify={async (code) => { await accountService.verifyEmailChange(address, code); setStep("done"); }} />
        ) : (
          <div className="space-y-4">
            <p className="text-sm">You now sign in with <span className="font-semibold">{address}</span>. We emailed your old address a link to undo this, valid for 7 days.</p>
            <Button className="h-11 w-full rounded-xl" onClick={() => { reset(false); window.location.reload(); }}>Done</Button>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
