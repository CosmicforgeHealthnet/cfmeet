"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Building2, Mail } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useWorkspace } from "@/context/WorkspaceContext";
import { useCreateBusinessOrganization } from "@/hooks/v2/useBusinessOrganizations";
import LegacyOrganizationsDirectory from "@/components/organizations/LegacyOrganizationsDirectory";
import { OrgError, Section, TextField } from "@/components/organizations/shared";
import {
  SLUG_PATTERN,
  newIdempotencyKey,
  provisioningLabel,
  suggestSlug,
} from "@/lib/organizations/organization-core";

/**
 * Organizations (Section 8/17 IA): the caller's organization workspaces from
 * `GET /api/v2/me/workspaces` (Personal excluded), create one, and the way to an invitation.
 * Until the Organizations Service is authoritative the legacy directory view is kept.
 */
export default function OrganizationsContent() {
  const { workspaces, authority, isLoading, listError } = useWorkspace();

  if (authority === "LEGACY_DIRECTORY") return <LegacyOrganizationsDirectory />;

  const organizations = workspaces.filter((w) => w.kind === "ORGANIZATION");

  return (
    <div className="mx-auto max-w-5xl space-y-6 p-6">
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Organizations</h1>
          <p className="text-sm text-muted-foreground">
            Business organizations you belong to. What you can manage in each one is decided by your role there.
          </p>
        </div>
        <Button asChild variant="outline" size="sm">
          <Link href="/organizations/invitations">
            <Mail className="h-4 w-4" /> Open an invitation
          </Link>
        </Button>
      </header>

      {isLoading && <p role="status">Loading your organizations…</p>}
      {listError ? <OrgError error={listError} /> : null}

      {!isLoading && !listError && organizations.length === 0 && (
        <p className="rounded-lg border border-border p-4 text-sm text-muted-foreground">
          You are not a member of any organization yet. Create one below, or accept an invitation you received by email.
        </p>
      )}

      {organizations.length > 0 && (
        <ul className="grid gap-3 sm:grid-cols-2">
          {organizations.map((w) => (
            <li key={w.id}>
              <Link
                href={`/dashboard/organizations/${encodeURIComponent(w.organizationId ?? w.id)}`}
                className="flex items-start gap-3 rounded-xl border border-border bg-card p-4 transition-colors hover:bg-muted"
              >
                <Building2 className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
                <span className="min-w-0">
                  <span className="block truncate font-semibold">{w.displayName}</span>
                  <span className="block text-xs text-muted-foreground">
                    {w.slug ? `${w.slug} · ` : ""}
                    {w.role ?? "Member"} · {w.organizationStatus ?? "ACTIVE"}
                    {w.provisioningState && w.provisioningState !== "READY" && w.provisioningState !== "NOT_REQUIRED"
                      ? ` · Setup: ${provisioningLabel(w.provisioningState)}`
                      : ""}
                  </span>
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}

      <CreateOrganization />
    </div>
  );
}

function CreateOrganization() {
  const router = useRouter();
  const create = useCreateBusinessOrganization();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);
  // One key per submission attempt: a retry of the same form reuses it (safe replay).
  const [idempotencyKey, setIdempotencyKey] = useState<string | null>(null);
  const effectiveSlug = slugEdited ? slug : suggestSlug(name);
  const nameProblem = name.trim().length === 0 || [...name.trim()].length > 200 ? "1–200 characters." : undefined;
  const slugProblem = SLUG_PATTERN.test(effectiveSlug) ? undefined : "3–63 lowercase letters, digits or hyphens (not at the ends).";

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
    <Section title="Create an organization" description="You become its owner. You can add a profile and invite people afterwards.">
      <form className="grid gap-3 sm:grid-cols-2" onSubmit={(e) => void submit(e)}>
        <TextField label="Organization name" value={name} onChange={(v) => edit(() => setName(v))} error={name ? nameProblem : undefined} required maxLength={200} />
        <TextField
          label="Address (slug)"
          value={effectiveSlug}
          onChange={(v) => edit(() => { setSlugEdited(true); setSlug(v.toLowerCase()); })}
          error={effectiveSlug ? slugProblem : undefined}
          hint="Lowercase letters, digits and hyphens."
          required
          maxLength={63}
        />
        {create.isError && (
          <div className="sm:col-span-2">
            <OrgError error={create.error} />
          </div>
        )}
        <div className="sm:col-span-2">
          <Button type="submit" disabled={create.isPending || !!nameProblem || !!slugProblem}>
            {create.isPending ? "Creating…" : "Create organization"}
          </Button>
        </div>
      </form>
    </Section>
  );
}
