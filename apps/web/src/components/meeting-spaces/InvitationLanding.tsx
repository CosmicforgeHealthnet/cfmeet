"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useResolveInvitation } from "@/hooks/v2/useInvitations";
import { invitationToken } from "@/lib/meeting-authority";
import type { ResolveInvitationResponse } from "@/types/v2";
import ConferenceRoom from "@/components/room/ConferenceRoom";
import { RoomMessage } from "@/components/room/pre-join-screen";
import { useResolveSpace } from "@/hooks/v2/useSpaces";
import { Spinner } from "@/components/ui/spinner";
import { Button } from "@/components/ui/button";
import { Field, RequestError } from "./ManagementUI";

export default function InvitationLanding() {
  const resolve = useResolveInvitation();
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<unknown>();
  const [reviewed, setReviewed] = useState<{ token: string; details: ResolveInvitationResponse } | null>(null);
  const [joining, setJoining] = useState(false);
  const readFragment = useRef(false);
  useEffect(() => {
    if (readFragment.current) return;
    readFragment.current = true;
    const fragment = window.location.hash.slice(1);
    if (fragment) {
      // Synchronize the external invitation fragment once, then remove it from the address bar.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setDraft(fragment);
      window.history.replaceState(window.history.state, "", window.location.pathname);
    }
  }, []);
  // The token stays in memory and goes with the join request; the room is keyed by conference id.
  if (joining && reviewed) return reviewed.details.conferenceId
    ? <ConferenceRoom conferenceId={reviewed.details.conferenceId} initialInvitationToken={reviewed.token} />
    : <SpaceInvitationRoom meetingCode={reviewed.details.meetingSpace.meetingCode} token={reviewed.token} />;
  return <main className="flex min-h-screen items-center justify-center bg-background px-4 py-10"><div className="w-full max-w-lg space-y-6 rounded-2xl border border-border bg-card p-6 sm:p-8">
    <div><h1 className="text-2xl font-bold">Meeting invitation</h1><p className="mt-2 text-sm text-muted-foreground">Review this invitation before joining. Reviewing does not consume a use or grant membership.</p></div>
    {!reviewed ? <form className="space-y-4" onSubmit={async event => {
      event.preventDefault(); setError(undefined);
      try { const token = invitationToken(draft); const details = await resolve.mutateAsync(token); setReviewed({ token, details }); setDraft(""); resolve.reset(); }
      catch (cause) { setError(cause); resolve.reset(); }
    }}><Field label="Invitation token or link" type="password" autoComplete="off" required value={draft} onChange={e => setDraft(e.target.value)} /><Button className="w-full" disabled={resolve.isPending} type="submit">{resolve.isPending ? "Reviewing…" : "Review invitation"}</Button></form> : <div className="space-y-4"><h2 className="text-lg font-semibold">{reviewed.details.meetingSpace.title || "Meeting"}</h2><p className="text-sm">Requested role: {reviewed.details.requestedRole.replaceAll("_", " ")}</p><p className="text-sm text-muted-foreground">Expires {new Date(reviewed.details.expiresAt).toLocaleString()} · {reviewed.details.remainingUses == null ? "Unlimited uses" : `${reviewed.details.remainingUses} uses remaining`}</p><p className="text-sm text-muted-foreground">{reviewed.details.conferenceId ? "This invitation is bound to one conference occurrence." : "This invitation can be presented for an occurrence in this space."} Admission policies and any verified-email requirement are checked when you join.</p><Button className="w-full" onClick={() => setJoining(true)}>Continue to pre-join</Button><Button variant="outline" className="w-full" onClick={() => setReviewed(null)}>Review another invitation</Button></div>}
    {error !== undefined && <RequestError error={error} />}
    <Button variant="ghost" asChild><Link href="/">Back to home</Link></Button>
  </div></main>;
}

/** A space-wide invitation joins the space's canonical conference. */
function SpaceInvitationRoom({ meetingCode, token }: { meetingCode: string; token: string }) {
  const resolved = useResolveSpace(meetingCode);
  if (resolved.isLoading) return <div className="flex min-h-screen items-center justify-center"><Spinner label="Opening meeting..." /></div>;
  const conferenceId = resolved.data?.conference?.id;
  if (!conferenceId) return <RoomMessage icon="ended" title="Nothing to join yet">This meeting has no occurrence to join right now. Ask the organizer for the meeting time.</RoomMessage>;
  return <ConferenceRoom conferenceId={conferenceId} initialInvitationToken={token} />;
}
