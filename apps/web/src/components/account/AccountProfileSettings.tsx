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
import { BadgeCheck, Check, Mail } from "lucide-react";
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
  const changed = Object.keys(patch).length > 0;
  const set = (field: keyof ProfileForm) => (e: React.ChangeEvent<HTMLInputElement>) => {
    setForm((f) => ({ ...f, [field]: e.target.value }));
    setSaved(false);
  };
  const error = update.isError ? apiErrorOf(update.error) : null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!changed || Object.keys(problems).length) return;
    try {
      await update.mutateAsync(patch);
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
        {avatar ? (
          // eslint-disable-next-line @next/next/no-img-element
          <img src={avatar} alt="" className="size-16 shrink-0 rounded-full object-cover" />
        ) : (
          <span className="flex size-16 shrink-0 items-center justify-center rounded-full bg-primary/10 text-xl font-semibold text-primary">{name.split(/\s+/).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("")}</span>
        )}
        <div className="min-w-0 flex-1">
          <p className="truncate text-lg font-semibold">{name}</p>
          <p className="flex items-center gap-1.5 truncate text-sm text-muted-foreground"><Mail className="size-3.5 shrink-0" />{profile.email}{profile.emailVerified ? <BadgeCheck className="size-4 shrink-0 text-primary" aria-label="Verified" /> : null}</p>
          <p className="mt-1.5 flex flex-wrap gap-1.5 text-xs">
            <span className="rounded-full bg-muted px-2 py-0.5 font-medium capitalize text-muted-foreground">Signs in with {profile.authType.toLowerCase() === "google" ? "Google" : "password"}</span>
            <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 font-medium text-emerald-700 dark:text-emerald-400">{accountStatusLabel(normalizeAccountStatus(profile.status))}</span>
            {!profile.emailVerified && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 font-medium text-amber-700 dark:text-amber-300">Email not verified</span>}
          </p>
        </div>
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
          <div className="sm:col-span-2">{field("avatarUrl", "Profile photo link", "Optional. A https:// link to an image.", "https://")}</div>
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
            <button type="button" onClick={() => setForm(profileForm(profile))} className="h-9 rounded-full px-4 text-sm font-semibold hover:bg-muted">Discard</button>
            <Button type="submit" className="rounded-full" loading={update.isPending} disabled={update.isPending || Object.keys(problems).length > 0}>Save changes</Button>
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

function DangerZone({ profile }: { profile: AccountProfile }) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  // ACC-026: password accounts confirm with their password; Google accounts rely on a recent sign-in.
  const passwordAccount = profile.authType.toLowerCase() !== "google";

  const deactivate = async () => {
    setPending(true);
    setError("");
    const sent = password.length > 0;
    try {
      await accountService.deactivate(sent ? password : null);
      setPassword("");
      await authService.forgetLocalSession();
      router.replace("/login?reason=deactivated");
    } catch (cause) {
      setPassword("");
      switch (deactivationOutcome(cause, sent)) {
        case "wrong_password":
          setError("That password is not correct.");
          break;
        case "reauthenticate":
          setError(passwordAccount ? "Enter your current password to confirm." : "For your security, sign out and sign in with Google again, then retry within 10 minutes.");
          break;
        default:
          setError(apiErrorOf(cause).message ?? "Your account could not be deactivated. Try again.");
      }
      setPending(false);
    }
  };

  return (
    <div className="rounded-2xl border border-destructive/30 bg-card p-5 sm:p-6">
      <div className="flex flex-col justify-between gap-4 sm:flex-row sm:items-center">
        <div className="space-y-1">
          <p className="font-semibold text-destructive">Deactivate account</p>
          <p className="text-sm text-muted-foreground">Signs you out everywhere and stops you signing in. Only CF Meet support can turn it back on.</p>
        </div>
        <Dialog
          open={open}
          onOpenChange={(value) => {
            if (pending) return;
            setOpen(value);
            setError("");
            setPassword("");
          }}
        >
          <DialogTrigger asChild>
            <Button variant="outline" className="w-full rounded-full border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive sm:w-auto">
              Deactivate
            </Button>
          </DialogTrigger>
          <DialogContent>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void deactivate();
              }}
              className="space-y-4"
            >
              <DialogHeader>
                <DialogTitle>Deactivate account</DialogTitle>
                <DialogDescription>
                  You will be signed out on every device and will not be able to sign in until CF Meet support reactivates the account.
                </DialogDescription>
              </DialogHeader>
              {passwordAccount ? (
                <div className="space-y-2">
                  <Label htmlFor="deactivate-password">Current password</Label>
                  <Input
                    id="deactivate-password"
                    type="password"
                    autoComplete="current-password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    required
                  />
                </div>
              ) : (
                <p className="text-sm text-muted-foreground">Google accounts confirm with a sign-in made in the last 10 minutes.</p>
              )}
              {error && (
                <p role="alert" className="text-sm text-destructive">
                  {error}
                </p>
              )}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={() => setOpen(false)} disabled={pending}>
                  Cancel
                </Button>
                <Button type="submit" variant="destructive" loading={pending} disabled={pending || (passwordAccount && !password)}>
                  Deactivate account
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      </div>
    </div>
  );
}
