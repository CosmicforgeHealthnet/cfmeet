"use client";

import { useEffect, useState } from "react";
import type { MeetingSpaceResponse, Role } from "@/types/v2";
import { useConferences } from "@/hooks/v2/useConferences";
import { useInvitations, useCreateInvitation, useRevokeInvitation } from "@/hooks/v2/useInvitations";
import { invitationInput, invitationRoles, meetingAllows, invitationAvailability } from "@/lib/meeting-authority";
import { joinable, schedulingError } from "@/lib/meeting-scheduling";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ActionDialog, Field, RequestError } from "./ManagementUI";

export default function InvitationsPanel({ space }: { space: MeetingSpaceResponse }) {
  const allowed = meetingAllows(space.viewerRole, "participant.admit", space.rolePolicy);
  const list = useInvitations(allowed ? space.id : "");
  const conferences = useConferences(allowed && space.status === "ACTIVE" ? space.id : "");
  const create = useCreateInvitation(space.id), revoke = useRevokeInvitation(space.id);
  const roles = invitationRoles(space.viewerRole, space.rolePolicy);
  const [role, setRole] = useState<Role>("PARTICIPANT");
  const [conferenceId, setConferenceId] = useState("");
  const [email, setEmail] = useState("");
  const [hours, setHours] = useState("168");
  const [limit, setLimit] = useState("1");
  const [secret, setSecret] = useState<{ token: string; id: string } | null>(null);
  const [copyState, setCopyState] = useState("");
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 30000); return () => clearInterval(timer); }, []);
  if (!allowed) return null;
  return <section className="space-y-4"><div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-lg font-semibold">Invitations</h2><p className="text-sm text-muted-foreground">Expiring access with a role and optional occurrence, verified email, and use limit. Membership stays separate.</p></div>
    {space.status === "ACTIVE" && roles.length > 0 && !secret && <ActionDialog label="Create invitation" description="The invitation link is shown once. Copy it before dismissing it. Creating an invitation does not send an email." submit={async () => {
      if (!roles.includes(role)) throw new Error("Choose a role within your grant authority.");
      const result = await create.mutateAsync(invitationInput({ role, conferenceId, email, hours, limit }));
      setSecret({ token: result.token, id: result.invitation.id }); setCopyState(""); create.reset();
    }}>
      <label className="grid gap-2 text-sm font-medium">Role<select className="h-10 rounded-md border border-input bg-background px-3" value={role} onChange={e => setRole(e.target.value as Role)}>{roles.map(item => <option key={item} value={item}>{item.replaceAll("_", " ")}</option>)}</select></label>
      <label className="grid gap-2 text-sm font-medium">Scope<select className="h-10 rounded-md border border-input bg-background px-3" value={conferenceId} onChange={e => setConferenceId(e.target.value)}><option value="">Any occurrence in this space</option>{conferences.data?.items.filter(c => joinable(c.status)).map(c => <option key={c.id} value={c.id}>{c.scheduledStartAt ? new Date(c.scheduledStartAt).toLocaleString() : c.id} · {c.status}</option>)}</select></label>
      {conferences.isError && <RequestError error={conferences.error} retry={() => void conferences.refetch()} />}
      <Field label="Verified email (optional)" type="email" value={email} onChange={e => setEmail(e.target.value)} maxLength={320} />
      <p className="text-xs text-muted-foreground">An email-bound invitation requires that exact verified account when joining.</p>
      <Field label="Expires in hours" type="number" min="1" step="1" required value={hours} onChange={e => setHours(e.target.value)} />
      <Field label="Use limit (empty for unlimited)" type="number" min="1" max="10000" step="1" value={limit} onChange={e => setLimit(e.target.value)} />
    </ActionDialog>}
  </div>
  {secret && <div className="space-y-3 rounded-xl border border-primary/40 bg-card p-4"><p className="font-medium">Copy this invitation link now</p><p className="text-sm text-muted-foreground">The server cannot retrieve this token again. Anyone with an unbound link can present it for admission.</p><Input aria-label="New invitation link" readOnly value={`${typeof window !== "undefined" ? window.location.origin : ""}/invite#${secret.token}`} /><div className="flex flex-wrap gap-2"><Button variant="outline" onClick={async () => { try { await navigator.clipboard.writeText(`${window.location.origin}/invite#${secret.token}`); setCopyState("Copied"); } catch (error) { setCopyState(schedulingError(error)); } }}>Copy link</Button><Button variant="outline" onClick={() => { setSecret(null); setCopyState(""); }}>Dismiss link</Button><ActionDialog label="Revoke new invitation" destructive description="Invalidate this invitation immediately." submit={async () => { await revoke.mutateAsync(secret.id); setSecret(null); }} /></div>{copyState && <p role="status" className="text-sm">{copyState}</p>}</div>}
  {list.isLoading && <p role="status">Loading invitations…</p>}
  {list.isError && <RequestError error={list.error} retry={() => void list.refetch()} />}
  {list.data?.length === 0 && <p className="text-sm text-muted-foreground">No invitations yet.</p>}
  {list.data?.map(inv => <article key={inv.id} className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-card p-4"><div className="min-w-0 space-y-1"><p className="font-medium">{inv.requestedRole.replaceAll("_", " ")} · {invitationAvailability(inv, now)}</p><p className="text-sm text-muted-foreground break-all">{inv.inviteeEmail || "Bearer invitation"} · {inv.conferenceId ? `Occurrence ${inv.conferenceId}` : "All occurrences"}</p><p className="text-xs text-muted-foreground">Expires {new Date(inv.expiresAt).toLocaleString()} · Used {inv.usageCount}{inv.useLimit != null ? ` / ${inv.useLimit}` : " (unlimited)"}</p></div>{inv.status === "ACTIVE" && <ActionDialog label="Revoke" destructive description="Prevent future use of this invitation. Existing participation remains separate." submit={async () => { await revoke.mutateAsync(inv.id); if (secret?.id === inv.id) setSecret(null); }} />}</article>)}
  </section>;
}
