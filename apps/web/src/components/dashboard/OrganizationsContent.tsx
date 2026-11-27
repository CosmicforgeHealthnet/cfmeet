"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Building2, ChevronRight, Mail, Plus } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useWorkspace } from "@/context/WorkspaceContext";
import { useAnswerOrganizationInvitation, useCreateBusinessOrganization, useMyOrganizationInvitations } from "@/hooks/v2/useBusinessOrganizations";
import LegacyOrganizationsDirectory from "@/components/organizations/LegacyOrganizationsDirectory";
import { OrgError, OrgMark, TextField } from "@/components/organizations/shared";
import {
  SLUG_PATTERN,
  newIdempotencyKey,
  provisioningLabel,
  suggestSlug,
} from "@/lib/organizations/organization-core";

/** Invitations addressed to me (CF Meet ID or verified email), answered right here. */
function PendingInvitations() {
  const invitations = useMyOrganizationInvitations();
  const answer = useAnswerOrganizationInvitation();
  const [error, setError] = useState<string | null>(null);
  const items = invitations.data ?? [];
  if (!items.length) return null;
  return (
    <section className="mt-6 rounded-2xl border border-primary/30 bg-primary/5 p-4 sm:p-5" data-testid="org-invitations">
      <h2 className="text-base font-semibold">Invitations for you</h2>
      <p className="mt-0.5 text-sm text-muted-foreground">Join a team or company that invited you.</p>
      {error ? <p role="alert" className="mt-2 text-sm text-destructive">{error}</p> : null}
      <ul className="mt-3 space-y-2">
        {items.map((inv) => {
          const name = inv.organization?.name ?? "An organization";
          const pending = answer.isPending && answer.variables?.id === inv.id;
          return (
            <li key={inv.id} className="flex flex-wrap items-center gap-3 rounded-xl bg-background p-3 ring-1 ring-border" data-testid="org-invitation">
              <OrgMark name={name} />
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">{name}</span>
                <span className="block text-xs text-muted-foreground">Join as {niceRole(inv.role)} · expires {new Date(inv.expiresAt).toLocaleDateString()}</span>
              </span>
              <span className="flex gap-2">
                <button type="button" disabled={pending} data-testid="decline-org-invitation"
                  onClick={() => { setError(null); answer.mutate({ id: inv.id, accept: false }, { onError: () => setError(`Couldn't decline the invitation from ${name}. Try again.`) }); }}
                  className="h-9 rounded-full px-4 text-sm font-semibold ring-1 ring-border hover:bg-muted disabled:opacity-60">Decline</button>
                <button type="button" disabled={pending} data-testid="accept-org-invitation"
                  onClick={() => { setError(null); answer.mutate({ id: inv.id, accept: true }, { onError: () => setError(`Couldn't join ${name}. The invitation may have expired.`) }); }}
                  className="h-9 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60">{pending ? "…" : "Join"}</button>
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

const niceRole = (role?: string | null) => (role ? role.charAt(0) + role.slice(1).toLowerCase().replaceAll("_", " ") : "Member");

/**
 * Organizations (Section 8/17 IA): the caller's organization workspaces from
 * `GET /api/v2/me/workspaces` (Personal excluded), create one, and the way to an invitation.
 * Until the Organizations Service is authoritative the legacy directory view is kept.
 */
export default function OrganizationsContent() {
  const { workspaces, authority, isLoading, listError } = useWorkspace();
  const [creating, setCreating] = useState(false);

  if (authority === "LEGACY_DIRECTORY") return <LegacyOrganizationsDirectory />;

  const organizations = workspaces.filter((w) => w.kind === "ORGANIZATION");

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Organizations</h1>
          <p className="mt-0.5 text-sm text-muted-foreground">Teams and companies you belong to.</p>
        </div>
        <div className="flex items-center gap-2">
          <Link href="/organizations/invitations" className="inline-flex h-10 items-center gap-2 rounded-full px-4 text-sm font-semibold text-foreground ring-1 ring-border hover:bg-muted"><Mail className="size-4" />Use an invite</Link>
          <button type="button" onClick={() => setCreating(true)} className="inline-flex h-10 items-center gap-2 rounded-full bg-primary pl-3.5 pr-4 text-sm font-semibold text-primary-foreground shadow-sm hover:bg-primary/90"><Plus className="size-4" />New organization</button>
        </div>
      </header>

      <PendingInvitations />

      <div className="mt-6">
        {isLoading && <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{[0, 1, 2].map((i) => <div key={i} className="h-24 animate-pulse rounded-2xl bg-muted" />)}</div>}
        {listError ? <OrgError error={listError} /> : null}

        {!isLoading && !listError && organizations.length === 0 && (
          <div className="flex flex-col items-center rounded-2xl border border-dashed border-border px-6 py-16 text-center">
            <span className="flex size-16 items-center justify-center rounded-2xl bg-violet-500/10 text-violet-600"><Building2 className="size-7" /></span>
            <p className="mt-4 text-lg font-semibold">No organizations yet</p>
            <p className="mt-1 max-w-sm text-sm text-muted-foreground">Create one for your team or company, or join with an invite you received by email.</p>
            <div className="mt-5 flex flex-wrap justify-center gap-2">
              <Link href="/organizations/invitations" className="inline-flex h-10 items-center gap-2 rounded-full px-5 text-sm font-semibold text-foreground ring-1 ring-border hover:bg-muted"><Mail className="size-4" />Use an invite</Link>
              <button type="button" onClick={() => setCreating(true)} className="inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" />New organization</button>
            </div>
          </div>
        )}

        {organizations.length > 0 && (
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {organizations.map((w) => {
              const settingUp = w.provisioningState && w.provisioningState !== "READY" && w.provisioningState !== "NOT_REQUIRED";
              const status = w.organizationStatus && w.organizationStatus !== "ACTIVE" ? niceRole(w.organizationStatus) : null;
              return (
                <li key={w.id}>
                  <Link href={`/dashboard/organizations/${encodeURIComponent(w.organizationId ?? w.id)}`}
                    className="group flex items-center gap-4 rounded-2xl border border-border bg-card p-4 transition-all hover:border-primary/40 hover:shadow-md">
                    <OrgMark name={w.displayName} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-semibold">{w.displayName}</span>
                      <span className="mt-1 flex flex-wrap items-center gap-1.5 text-xs">
                        <span className="rounded-full bg-muted px-2 py-0.5 font-medium text-muted-foreground">{niceRole(w.role)}</span>
                        {status && <span className="rounded-full bg-amber-500/15 px-2 py-0.5 font-medium text-amber-700 dark:text-amber-300">{status}</span>}
                        {settingUp && <span className="rounded-full bg-primary/10 px-2 py-0.5 font-medium text-primary">Setting up · {provisioningLabel(w.provisioningState)}</span>}
                      </span>
                    </span>
                    <ChevronRight className="size-5 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
                  </Link>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <CreateOrganization open={creating} onOpenChange={setCreating} />
    </div>
  );
}

function CreateOrganization({ open, onOpenChange }: { open: boolean; onOpenChange: (v: boolean) => void }) {
  const router = useRouter();
  const create = useCreateBusinessOrganization();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);
  // One key per submission attempt: a retry of the same form reuses it (safe replay).
  const [idempotencyKey, setIdempotencyKey] = useState<string | null>(null);
  const effectiveSlug = slugEdited ? slug : suggestSlug(name);
  const nameProblem = name.trim().length === 0 || [...name.trim()].length > 200 ? "Enter 1–200 characters." : undefined;
  const slugProblem = SLUG_PATTERN.test(effectiveSlug) ? undefined : "3–63 lowercase letters, numbers or hyphens (no hyphen at the start or end).";

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (nameProblem || slugProblem) return;
    const key = idempotencyKey ?? newIdempotencyKey();
    setIdempotencyKey(key);
    try {
      const org = await create.mutateAsync({ body: { name: name.trim(), slug: effectiveSlug }, idempotencyKey: key });
      setIdempotencyKey(null);
      router.push(`/dashboard/organizations/${encodeURIComponent(org.id)}`);
    } catch {
      /* shown below; the key is kept so "Create" again is a safe retry */
    }
  };

  const edit = (apply: () => void) => {
    apply();
    setIdempotencyKey(null); // different details: a new request
    create.reset();
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader className="text-left">
          <DialogTitle>New organization</DialogTitle>
          <DialogDescription>You&apos;ll be the owner. Add details and invite people after.</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={(e) => void submit(e)}>
          <div className="flex items-center gap-3 rounded-2xl bg-muted/50 p-3">
            <OrgMark name={name.trim() || "?"} />
            <div className="min-w-0">
              <p className="truncate font-semibold">{name.trim() || "Your organization"}</p>
              <p className="truncate text-xs text-muted-foreground">Address: {effectiveSlug || "—"}</p>
            </div>
          </div>
          <TextField label="Name" placeholder="e.g. Cosmic Forge Health" value={name} onChange={(v) => edit(() => setName(v))} error={name ? nameProblem : undefined} required maxLength={200} />
          <TextField
            label="Address"
            value={effectiveSlug}
            onChange={(v) => edit(() => { setSlugEdited(true); setSlug(v.toLowerCase()); })}
            error={effectiveSlug ? slugProblem : undefined}
            hint="Made from the name. Lowercase letters, numbers and hyphens."
            required
            maxLength={63}
          />
          {create.isError && <OrgError error={create.error} />}
          <div className="flex justify-end gap-2 pt-1">
            <button type="button" onClick={() => onOpenChange(false)} className="h-10 rounded-full px-4 text-sm font-semibold text-foreground hover:bg-muted">Cancel</button>
            <button type="submit" disabled={create.isPending || !!nameProblem || !!slugProblem} className="h-10 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{create.isPending ? "Creating…" : "Create"}</button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
