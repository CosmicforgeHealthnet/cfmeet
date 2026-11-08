"use client";

/**
 * Resolves a connect invite (`POST /discovery/invites/resolve`) and lets the signed-in person
 * send a contact request. The token is a secret: kept in memory, never logged or stored, and
 * an expired / revoked / used invite looks exactly like an unknown one (neutral NOT_FOUND).
 */
import { useEffect, useState } from "react";
import Link from "next/link";
import { useMutation } from "@tanstack/react-query";
import { UserPlus } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { communicationsErrorMessage } from "@/lib/communications/errors";
import { inviteTokenFromPayload } from "@/lib/communications/comms-core";

export default function ConnectInvite({ token }: { token: string }) {
  const valid = inviteTokenFromPayload(token);
  const resolve = useMutation({ mutationFn: (t: string) => hub.resolveInvite(t), retry: false });
  const request = useMutation({ mutationFn: (cfMeetId: string) => hub.requestContact(cfMeetId), retry: false });
  const [started, setStarted] = useState(false);
  useEffect(() => {
    if (valid && !started) {
      setStarted(true);
      resolve.mutate(valid);
    }
  }, [valid, started, resolve]);
  const unauthenticated = (resolve.error as { status?: number } | null)?.status === 401;
  const profile = resolve.data;
  return (
    <main className="flex min-h-screen items-center justify-center bg-muted p-4" data-testid="connect-page">
      <div className="w-full max-w-md space-y-4 rounded-2xl bg-background p-8 shadow-lg">
        <h1 className="text-xl font-bold">Connect on CF Meet</h1>
        {!valid ? <p role="alert" className="text-sm text-destructive">This invite link is not valid.</p> : null}
        {resolve.isPending ? <p className="flex items-center gap-2 text-sm"><Spinner /> Checking the invite…</p> : null}
        {unauthenticated ? (
          <p className="text-sm">Sign in to connect. <Link className="text-primary underline" href="/login">Sign in</Link>, then open this link again.</p>
        ) : resolve.error ? (
          <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(resolve.error)}</p>
        ) : null}
        {profile ? (
          <div className="space-y-3" data-testid="invite-profile">
            <div>
              <p className="font-medium">{profile.displayName}</p>
              <p className="font-mono text-xs text-muted-foreground">{profile.cfMeetId}</p>
            </div>
            {profile.relationship === "CONNECTED" ? (
              <p className="text-sm">You are already connected. <Link className="text-primary underline" href="/dashboard/contacts">Open contacts</Link></p>
            ) : (
              <Button data-testid="invite-request" disabled={request.isPending || request.isSuccess || profile.permittedActions?.requestConnection === false} onClick={() => request.mutate(profile.cfMeetId)}>
                <UserPlus className="h-4 w-4" /> {request.isSuccess ? "Request sent" : "Send contact request"}
              </Button>
            )}
            {request.error ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(request.error)}</p> : null}
            {request.isSuccess ? <p className="text-sm text-muted-foreground">They will see your request in their Contacts. <Link className="text-primary underline" href="/dashboard/contacts">Go to contacts</Link></p> : null}
          </div>
        ) : null}
      </div>
    </main>
  );
}
