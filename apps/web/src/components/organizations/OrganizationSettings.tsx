"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  useBusinessOrganizationMembers,
  useOrganizationLifecycle,
  useTransferOwnership,
  useUpdateBusinessOrganization,
} from "@/hooks/v2/useBusinessOrganizations";
import {
  ORG_CAPABILITIES,
  legalVerificationLabel,
  lifecycleActions,
  memberRole,
  needsReload,
  organizationForm,
  organizationFormProblems,
  organizationPatch,
  orgCan,
  shortId,
  type OrganizationForm,
} from "@/lib/organizations/organization-core";
import type { BusinessOrganization } from "@/types/generated/meeting-service";
import { OrgError, Section, TextField } from "./shared";

export default function OrganizationSettings({ org }: { org: BusinessOrganization }) {
  return (
    <div className="space-y-4">
      {/* Remount the form when the authoritative version changes (save, conflict reload). */}
      {orgCan(org, ORG_CAPABILITIES.profileWrite) && <ProfileForm key={`${org.id}:${org.version}`} org={org} />}
      {orgCan(org, ORG_CAPABILITIES.ownershipTransfer) && <OwnershipTransfer org={org} />}
      {orgCan(org, ORG_CAPABILITIES.lifecycleManage) && <Lifecycle org={org} />}
    </div>
  );
}

/** `PATCH /organizations/{id}` with `expectedVersion` (VERSION_CONFLICT reloads the latest). */
function ProfileForm({ org }: { org: BusinessOrganization }) {
  const update = useUpdateBusinessOrganization(org.id);
  const [form, setForm] = useState<OrganizationForm>(() => organizationForm(org));
  const [saved, setSaved] = useState(false);
  const problems = organizationFormProblems(form);
  const patch = organizationPatch(org, form);
  const set = (field: keyof OrganizationForm) => (value: string) => {
    setForm((f) => ({ ...f, [field]: value }));
    setSaved(false);
  };

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!patch || Object.keys(problems).length) return;
    try {
      await update.mutateAsync(patch);
      setSaved(true);
    } catch {
      /* shown below */
    }
  };

  return (
    <Section title="Organization profile" description="Shown to members. Legal details are self-declared unless CF Meet verified them.">
      <form className="grid gap-3 sm:grid-cols-2" onSubmit={(e) => void submit(e)}>
        <TextField label="Name" value={form.name} onChange={set("name")} error={problems.name} required maxLength={200} />
        <TextField label="Address (slug)" value={form.slug} onChange={set("slug")} error={problems.slug} required maxLength={63} />
        <div className="sm:col-span-2">
          <TextField label="Description" value={form.description} onChange={set("description")} error={problems.description} maxLength={2000} multiline />
        </div>
        <TextField label="Industry" value={form.industry} onChange={set("industry")} error={problems.industry} maxLength={60} />
        <TextField label="Organization type" value={form.organizationType} onChange={set("organizationType")} error={problems.organizationType} maxLength={60} />
        <TextField label="Public contact email" type="email" value={form.publicContactEmail} onChange={set("publicContactEmail")} error={problems.publicContactEmail} maxLength={254} />
        <TextField label="City" value={form.city} onChange={set("city")} error={problems.city} maxLength={120} />
        <TextField label="Country code" value={form.countryCode} onChange={set("countryCode")} error={problems.countryCode} hint="ISO 3166-1 alpha-2, e.g. GB" maxLength={2} />
        <TextField label="Operating address" value={form.operatingAddress} onChange={set("operatingAddress")} error={problems.operatingAddress} maxLength={500} />
        <fieldset className="grid gap-3 rounded-md border border-border p-3 sm:col-span-2 sm:grid-cols-3">
          <legend className="px-1 text-sm font-medium">
            Legal registration · {legalVerificationLabel(org.profile?.legalRegistration?.verification)}
          </legend>
          <TextField label="Registered name" value={form.registeredName} onChange={set("registeredName")} error={problems.registeredName} maxLength={200} />
          <TextField label="Registration number" value={form.registrationNumber} onChange={set("registrationNumber")} error={problems.registrationNumber} maxLength={100} />
          <TextField label="Incorporated on" type="date" value={form.incorporatedOn} onChange={set("incorporatedOn")} error={problems.incorporatedOn} />
        </fieldset>
        {update.isError && (
          <div className="sm:col-span-2">
            <OrgError error={update.error} />
            {needsReload(update.error) && <p className="mt-1 text-xs text-muted-foreground">Your unsaved edits were replaced by the latest version.</p>}
          </div>
        )}
        {saved && !update.isError && <p role="status" className="text-sm text-green-700 sm:col-span-2">Saved.</p>}
        <div className="sm:col-span-2">
          <Button type="submit" disabled={!patch || update.isPending || Object.keys(problems).length > 0}>
            {update.isPending ? "Saving…" : "Save profile"}
          </Button>
        </div>
      </form>
    </Section>
  );
}

function OwnershipTransfer({ org }: { org: BusinessOrganization }) {
  const members = useBusinessOrganizationMembers(org.id, true);
  const transfer = useTransferOwnership(org.id);
  const [target, setTarget] = useState("");
  const [confirm, setConfirm] = useState(false);
  const candidates = (members.data ?? []).filter((m) => m.status === "ACTIVE" && m.id !== org.viewer?.membershipId && memberRole(m) !== "OWNER");

  const run = async () => {
    try {
      await transfer.mutateAsync({ toMembershipId: target, expectedVersion: org.version });
      setConfirm(false);
      setTarget("");
    } catch {
      /* shown below */
    }
  };

  return (
    <Section title="Transfer ownership" description="The member you choose becomes an owner and you become an admin.">
      {members.isError && <OrgError error={members.error} retry={() => void members.refetch()} />}
      {transfer.isError && <OrgError error={transfer.error} />}
      {transfer.isSuccess && <p role="status" className="text-sm">Ownership transferred.</p>}
      <div className="flex flex-wrap items-end gap-2">
        <label className="grid gap-1 text-sm">
          <span className="font-medium">New owner</span>
          <select className="h-10 min-w-56 rounded-md border border-input bg-background px-2" value={target} onChange={(e) => { setTarget(e.target.value); setConfirm(false); }}>
            <option value="">Choose a member…</option>
            {candidates.map((m) => (
              <option key={m.id} value={m.id}>
                Account {shortId(m.userId)} · {memberRole(m)}
              </option>
            ))}
          </select>
        </label>
        {confirm ? (
          <>
            <Button variant="destructive" size="sm" disabled={transfer.isPending} onClick={() => void run()}>
              {transfer.isPending ? "Transferring…" : "Confirm transfer"}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setConfirm(false)}>
              Cancel
            </Button>
          </>
        ) : (
          <Button size="sm" variant="outline" disabled={!target} onClick={() => setConfirm(true)}>
            Transfer…
          </Button>
        )}
      </div>
    </Section>
  );
}

const LIFECYCLE_TEXT = {
  suspend: { label: "Suspend organization", detail: "Pauses organization meetings, invitations and colleague lists. Owners can still view it and reactivate it." },
  reactivate: { label: "Reactivate organization", detail: "Restores members' access. A suspension imposed by CF Meet can only be lifted by CF Meet." },
  archive: { label: "Archive organization", detail: "Final for members: organization meetings lose organization access. Only owners can still view it." },
} as const;

function Lifecycle({ org }: { org: BusinessOrganization }) {
  const lifecycle = useOrganizationLifecycle(org.id);
  const [pending, setPending] = useState<"suspend" | "reactivate" | "archive" | null>(null);
  const actions = lifecycleActions(org.lifecycleStatus);

  const run = async () => {
    if (!pending) return;
    try {
      await lifecycle.mutateAsync({ action: pending, expectedVersion: org.version });
      setPending(null);
    } catch {
      /* shown below */
    }
  };

  return (
    <Section title="Organization status" description={`Current status: ${org.lifecycleStatus}.`}>
      {lifecycle.isError && <OrgError error={lifecycle.error} />}
      {actions.length === 0 && <p className="text-sm text-muted-foreground">No status change is available.</p>}
      <div className="space-y-3">
        {actions.map((a) => (
          <div key={a} className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border p-3">
            <div className="min-w-0 text-sm">
              <p className="font-medium">{LIFECYCLE_TEXT[a].label}</p>
              <p className="text-muted-foreground">{LIFECYCLE_TEXT[a].detail}</p>
            </div>
            {pending === a ? (
              <div className="flex gap-2">
                <Button size="sm" variant={a === "reactivate" ? "default" : "destructive"} disabled={lifecycle.isPending} onClick={() => void run()}>
                  {lifecycle.isPending ? "Working…" : "Confirm"}
                </Button>
                <Button size="sm" variant="outline" onClick={() => setPending(null)}>
                  Cancel
                </Button>
              </div>
            ) : (
              <Button size="sm" variant="outline" onClick={() => setPending(a)}>
                {LIFECYCLE_TEXT[a].label}…
              </Button>
            )}
          </div>
        ))}
      </div>
    </Section>
  );
}
