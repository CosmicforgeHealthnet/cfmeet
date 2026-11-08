"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { useCreateInvitation, useOrganizationInvitations, useRevokeInvitation } from "@/hooks/v2/useBusinessOrganizations";
import { invitableRoles, newIdempotencyKey } from "@/lib/organizations/organization-core";
import type { BusinessOrganization, OrganizationInvitation } from "@/types/generated/meeting-service";
import { OrgError, Section, TextField, formatDateTime } from "./shared";

const STATUSES = ["", "PENDING", "ACCEPTED", "DECLINED", "REVOKED", "EXPIRED"] as const;

/**
 * Invitations (`organization.members.invite`). The one-time token of a new invitation is shown
 * once, as a link with the token in the fragment (never sent to a server, never logged), and is
 * dropped from memory when the panel closes.
 */
export default function OrganizationInvitations({ org }: { org: BusinessOrganization }) {
  const [status, setStatus] = useState<string>("PENDING");
  const invitations = useOrganizationInvitations(org.id, status, true);
  const revoke = useRevokeInvitation(org.id);
  const suspended = org.lifecycleStatus !== "ACTIVE";

  return (
    <div className="space-y-4">
      {suspended ? (
        <p className="rounded-lg border border-border p-4 text-sm text-muted-foreground">Invitations are paused while the organization is not active.</p>
      ) : (
        <InviteForm org={org} />
      )}
      <Section
        title="Invitations"
        actions={
          <select
            aria-label="Invitation status"
            className="h-9 rounded-md border border-input bg-background px-2 text-sm"
            value={status}
            onChange={(e) => setStatus(e.target.value)}
          >
            {STATUSES.map((s) => (
              <option key={s} value={s}>
                {s || "All"}
              </option>
            ))}
          </select>
        }
      >
        {invitations.isLoading && <p role="status" className="text-sm">Loading invitations…</p>}
        {invitations.isError && <OrgError error={invitations.error} retry={() => void invitations.refetch()} />}
        {revoke.isError && <OrgError error={revoke.error} />}
        {invitations.data && invitations.data.length === 0 && <p className="text-sm text-muted-foreground">No invitations.</p>}
        {invitations.data && invitations.data.length > 0 && (
          <ul className="divide-y divide-border rounded-md border border-border">
            {invitations.data.map((inv: OrganizationInvitation) => (
              <li key={inv.id} className="flex flex-wrap items-center justify-between gap-3 p-3">
                <div className="min-w-0 text-sm">
                  <p className="font-medium">{inv.inviteeEmailHint}</p>
                  <p className="text-muted-foreground">
                    {inv.role} · {inv.status} · sent {formatDateTime(inv.createdAt)} · expires {formatDateTime(inv.expiresAt)}
                  </p>
                </div>
                {inv.status === "PENDING" && (
                  <Button size="sm" variant="outline" disabled={revoke.isPending} onClick={() => revoke.mutate(inv.id)}>
                    Revoke
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}

function InviteForm({ org }: { org: BusinessOrganization }) {
  const create = useCreateInvitation(org.id);
  const roles = invitableRoles(org.viewer?.role);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<string>(roles.includes("MEMBER") ? "MEMBER" : roles[0] ?? "MEMBER");
  const [sendEmail, setSendEmail] = useState(true);
  const [key, setKey] = useState<string | null>(null);
  const [issued, setIssued] = useState<{ email: string; link: string | null; delivery: string | null } | null>(null);
  const emailOk = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()) && email.trim().length <= 254;

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!emailOk) return;
    const k = key ?? newIdempotencyKey();
    setKey(k);
    try {
      const inv = await create.mutateAsync({ body: { email: email.trim(), role, sendEmail }, idempotencyKey: k });
      setKey(null);
      const link = inv.token ? `${window.location.origin}/organizations/invitations#${inv.token}` : null;
      setIssued({ email: inv.inviteeEmailHint, link, delivery: inv.emailDelivery ?? null });
      setEmail("");
    } catch {
      /* shown below; same key on retry */
    }
  };

  return (
    <Section title="Invite someone" description="Only the account whose verified email matches can accept.">
      <form className="grid gap-3 sm:grid-cols-[1fr_auto]" onSubmit={(e) => void submit(e)}>
        <TextField label="Email address" type="email" value={email} onChange={(v) => { setEmail(v); setKey(null); create.reset(); }} required maxLength={254} />
        <div className="grid gap-1 text-sm">
          <label htmlFor="invite-role" className="font-medium">
            Role
          </label>
          <select id="invite-role" className="h-10 rounded-md border border-input bg-background px-2" value={role} onChange={(e) => { setRole(e.target.value); setKey(null); }}>
            {roles.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </div>
        <label className="flex items-center gap-2 text-sm sm:col-span-2">
          <input type="checkbox" checked={sendEmail} onChange={(e) => { setSendEmail(e.target.checked); setKey(null); }} />
          Email the invitation link
        </label>
        {create.isError && (
          <div className="sm:col-span-2">
            <OrgError error={create.error} />
          </div>
        )}
        <div className="sm:col-span-2">
          <Button type="submit" disabled={!emailOk || create.isPending || roles.length === 0}>
            {create.isPending ? "Sending…" : "Send invitation"}
          </Button>
        </div>
      </form>
      {issued && (
        <div role="status" className="space-y-2 rounded-md border border-border bg-muted p-3 text-sm">
          <p>
            Invitation created for {issued.email}.
            {issued.delivery === "ACCEPTED" && " The link was emailed."}
            {issued.delivery === "FAILED" && " The email could not be sent — share the link below instead."}
            {issued.delivery === "DISABLED" && " Email is not enabled on this deployment — share the link below."}
          </p>
          {issued.link && (
            <>
              <p className="text-xs text-muted-foreground">This link is shown only once. Share it only with the invitee.</p>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="outline" onClick={() => void navigator.clipboard?.writeText(issued.link ?? "")}>
                  Copy invitation link
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setIssued(null)}>
                  Done
                </Button>
              </div>
            </>
          )}
        </div>
      )}
    </Section>
  );
}
