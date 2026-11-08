"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
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
      <div className="flex h-full items-center justify-center">
        <Spinner />
      </div>
    );
  }
  if (profile.isError || !profile.data) {
    return (
      <div role="alert" className="rounded-lg border border-destructive/30 p-3 text-sm text-destructive">
        Your profile could not be loaded.{" "}
        <Button size="sm" variant="outline" onClick={() => void profile.refetch()}>
          Retry
        </Button>
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
    <div className="space-y-1">
      <Label htmlFor={`profile-${id}`}>{label}</Label>
      <Input id={`profile-${id}`} value={form[id]} onChange={set(id)} placeholder={placeholder} aria-invalid={!!problems[id]} />
      {problems[id] ? <p className="text-xs text-destructive">{problems[id]}</p> : hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );

  return (
    <div className="space-y-6 rounded-2xl border border-border bg-card p-6 shadow-sm">
      <div>
        <h2 className="mb-1 text-lg font-semibold">Profile</h2>
        <p className="text-sm text-muted-foreground">Other people see your display name ({profile.effectiveDisplayName}).</p>
      </div>
      {error && (
        <div role="alert" className="rounded-lg border border-red-100 bg-red-50 px-4 py-3 text-sm text-red-600">
          {error.code === "USERNAME_TAKEN" ? "That username is already taken." : error.message ?? "Your profile could not be saved."}
        </div>
      )}
      {saved && (
        <div role="status" className="rounded-lg border border-green-100 bg-green-50 px-4 py-3 text-sm text-green-600">
          Profile saved.
        </div>
      )}
      <form onSubmit={(e) => void submit(e)} className="grid max-w-2xl gap-4 sm:grid-cols-2">
        <div className="space-y-1 sm:col-span-2">
          <Label htmlFor="profile-email">Email address</Label>
          <Input id="profile-email" value={profile.email} disabled className="bg-muted text-muted-foreground" />
          <p className="text-xs text-muted-foreground">{profile.emailVerified ? "Verified." : "Not verified yet."} Email cannot be changed here.</p>
        </div>
        {field("username", "Username", "3–50 letters, digits or underscores.")}
        {field("displayName", "Display name", "Optional, up to 64 characters.")}
        {field("timezone", "Time zone", "IANA name, e.g. Europe/London. Empty: your device's zone.", "Europe/London")}
        {field("locale", "Language", "e.g. en or en-GB. Empty: your device's language.", "en")}
        <div className="sm:col-span-2">{field("avatarUrl", "Avatar image URL", "Optional https:// address.", "https://")}</div>
        <div className="grid grid-cols-2 gap-4 sm:col-span-2">
          <div className="space-y-1">
            <Label>Sign-in method</Label>
            <div className="rounded-md border border-border bg-muted px-3 py-2 text-sm capitalize">{profile.authType}</div>
          </div>
          <div className="space-y-1">
            <Label>Status</Label>
            <div className="rounded-md border border-border bg-muted px-3 py-2 text-sm">{accountStatusLabel(normalizeAccountStatus(profile.status))}</div>
          </div>
        </div>
        <div className="sm:col-span-2">
          <Button type="submit" loading={update.isPending} disabled={update.isPending || !changed || Object.keys(problems).length > 0}>
            Save changes
          </Button>
        </div>
      </form>
    </div>
  );
}

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
    <div className="space-y-4 rounded-2xl border border-red-100 bg-red-50 p-6 dark:border-red-900 dark:bg-red-950/40">
      <div>
        <h2 className="mb-1 text-lg font-semibold text-red-700">Danger zone</h2>
      </div>
      <div className="flex flex-col justify-between gap-4 rounded-xl border border-red-100 bg-card p-4 sm:flex-row sm:items-center">
        <div className="space-y-1">
          <p className="font-medium">Deactivate account</p>
          <p className="text-sm text-muted-foreground">Signs you out everywhere and stops you from signing in. Reactivation needs CF Meet support.</p>
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
            <Button variant="destructive" className="w-full sm:w-auto">
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
