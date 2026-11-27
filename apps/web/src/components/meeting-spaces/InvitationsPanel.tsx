"use client";

import { NiceSelect } from "@/components/ui/nice-select";
import { useEffect, useState } from "react";
import type { MeetingSpaceResponse, Role } from "@/types/v2";
import { useConferences } from "@/hooks/v2/useConferences";
import { useInvitations, useCreateInvitation, useRevokeInvitation } from "@/hooks/v2/useInvitations";
import { invitationInput, invitationRoles, meetingAllows, invitationAvailability } from "@/lib/meeting-authority";
import { joinable, schedulingError } from "@/lib/meeting-scheduling";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ActionDialog, Field, RequestError } from "./ManagementUI";
import { Check, Copy, Link2, Mail, Ticket } from "lucide-react";

const nice = (v: string) => v.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase());

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
  return <section className="space-y-4"><div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="text-base font-semibold text-foreground">Invitations</h2><p className="text-sm text-muted-foreground">Invite links that expire, with a role and an optional email or use limit.</p></div>
    {space.status === "ACTIVE" && roles.length > 0 && !secret && <ActionDialog label="Create invite link" description="The link is shown only once, so copy it before closing. This doesn't send an email; share the link yourself." submit={async () => {
      if (!roles.includes(role)) throw new Error("Choose a role within your grant authority.");
      const result = await create.mutateAsync(invitationInput({ role, conferenceId, email, hours, limit }));
      setSecret({ token: result.token, id: result.invitation.id }); setCopyState(""); create.reset();
    }}>
      <NiceSelect label="Joins as" value={role} onChange={setRole} options={roles.map(item => ({ value: item, label: nice(item) }))} />
      <NiceSelect label="For" value={conferenceId} onChange={setConferenceId} options={[{ value: "", label: "Any session of this meeting" }, ...(conferences.data?.items.filter(c => joinable(c.status)).map(c => ({ value: c.id, label: c.scheduledStartAt ? new Date(c.scheduledStartAt).toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "numeric", minute: "2-digit" }) : "Session without a set time", hint: nice(c.status) })) ?? [])]} />
      {conferences.isError && <RequestError error={conferences.error} retry={() => void conferences.refetch()} />}
      <Field label="Only for this email (optional)" type="email" value={email} onChange={e => setEmail(e.target.value)} maxLength={320} />
      <p className="-mt-2 text-xs text-muted-foreground">If set, only a CF Meet account with that verified email can use the link.</p>
      <Field label="Expires after (hours)" type="number" min="1" step="1" required value={hours} onChange={e => setHours(e.target.value)} />
      <Field label="Number of uses (empty = unlimited)" type="number" min="1" max="10000" step="1" value={limit} onChange={e => setLimit(e.target.value)} />
    </ActionDialog>}
  </div>
  {secret && <div className="space-y-3 rounded-2xl border border-primary/30 bg-primary/5 p-5">
    <div className="flex items-start gap-3"><span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground"><Link2 className="size-4" /></span><div><p className="font-semibold text-foreground">Copy your invite link now</p><p className="text-sm text-muted-foreground">For security it&apos;s shown only once. Anyone with it can ask to join (unless it&apos;s tied to an email).</p></div></div>
    <div className="flex items-center gap-2 rounded-xl border border-border bg-card px-3 py-2"><Input aria-label="New invitation link" readOnly value={`${typeof window !== "undefined" ? window.location.origin : ""}/invite#${secret.token}`} className="h-9 border-0 px-0 font-mono text-xs focus:ring-0 sm:h-9 sm:px-0" />
      <Button className="h-9 shrink-0 rounded-lg shadow-none" onClick={async () => { try { await navigator.clipboard.writeText(`${window.location.origin}/invite#${secret.token}`); setCopyState("Copied"); } catch (error) { setCopyState(schedulingError(error)); } }}>{copyState === "Copied" ? <Check className="size-4" /> : <Copy className="size-4" />}{copyState === "Copied" ? "Copied" : "Copy"}</Button></div>
    <div className="flex flex-wrap gap-2"><Button variant="outline" className="h-9 rounded-xl" onClick={() => { setSecret(null); setCopyState(""); }}>Done</Button><ActionDialog label="Cancel this link" destructive description="Stop this invite link from working right away." submit={async () => { await revoke.mutateAsync(secret.id); setSecret(null); }} /></div>
    {copyState && copyState !== "Copied" && <p role="status" className="text-sm text-destructive">{copyState}</p>}
  </div>}
  {list.isLoading && <div aria-busy="true" className="h-20 animate-pulse rounded-2xl bg-muted" />}
  {list.isError && <RequestError error={list.error} retry={() => void list.refetch()} />}
  {list.data?.length === 0 && <p className="rounded-xl bg-muted/50 px-4 py-3 text-sm text-muted-foreground">No invite links yet. Create one to bring people in with a set role.</p>}
  {list.data?.map(inv => <article key={inv.id} className="flex flex-wrap items-center gap-3 rounded-2xl border border-border bg-card p-4">
    <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">{inv.inviteeEmail ? <Mail className="size-4" /> : <Ticket className="size-4" />}</span>
    <div className="min-w-0 flex-1 space-y-1">
      <div className="flex flex-wrap items-center gap-2"><p className="truncate font-medium text-foreground">{inv.inviteeEmail || "Anyone with the link"}</p><span className="rounded-md bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">{nice(inv.requestedRole)}</span><span className="rounded-md bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">{invitationAvailability(inv, now)}</span></div>
      <p className="text-xs text-muted-foreground">{inv.conferenceId ? "One session" : "Any session"} · Expires {new Date(inv.expiresAt).toLocaleString(undefined, { day: "numeric", month: "short", hour: "numeric", minute: "2-digit" })} · Used {inv.usageCount}{inv.useLimit != null ? ` of ${inv.useLimit}` : ""}</p>
    </div>
    {inv.status === "ACTIVE" && <ActionDialog label="Cancel link" destructive description="Stop this invite link from being used again. People already in keep their access." submit={async () => { await revoke.mutateAsync(inv.id); if (secret?.id === inv.id) setSecret(null); }} />}
  </article>)}
  </section>;
}
