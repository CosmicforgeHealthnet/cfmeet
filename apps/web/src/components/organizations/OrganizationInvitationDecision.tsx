"use client";

import { useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { accountSession } from "@/lib/auth/session";
import { useInvitationDecision } from "@/hooks/v2/useBusinessOrganizations";
import { errorEnvelopeOf, invitationTokenFromHash, organizationErrorText } from "@/lib/organizations/organization-core";

/**
 * `/organizations/invitations#<token>` (the link of the invitation email, Section 11).
 *
 * The token arrives in the URL fragment, which browsers never send to a server. It is removed
 * from the address bar and history immediately, kept in memory, and posted only in the body of
 * `POST /api/v2/organization-invitations/accept|decline`. While the visitor signs in it waits in
 * this tab's sessionStorage for at most 30 minutes, and is erased as soon as it is decided.
 */
const PENDING_KEY = "cfmeet.organization-invitation";
const PENDING_TTL_MS = 30 * 60_000;

function stash(token: string) {
  try {
    window.sessionStorage.setItem(PENDING_KEY, JSON.stringify({ token, until: Date.now() + PENDING_TTL_MS }));
  } catch {
    /* storage disabled: the visitor opens the link again after signing in */
  }
}

function unstash(): string | null {
  try {
    const raw = window.sessionStorage.getItem(PENDING_KEY);
    if (!raw) return null;
    const value = JSON.parse(raw) as { token?: unknown; until?: unknown };
    if (typeof value.until !== "number" || value.until < Date.now()) {
      window.sessionStorage.removeItem(PENDING_KEY);
      return null;
    }
    return invitationTokenFromHash(typeof value.token === "string" ? value.token : null);
  } catch {
    return null;
  }
}

function forget() {
  try {
    window.sessionStorage.removeItem(PENDING_KEY);
  } catch {
    /* nothing stored */
  }
}

/** Reads the token once per page load and erases it from the URL. */
function takeToken(): string | null {
  const fromHash = invitationTokenFromHash(window.location.hash);
  if (window.location.hash) window.history.replaceState(null, "", window.location.pathname);
  return fromHash ?? unstash();
}

const noop = () => () => {};

export default function OrganizationInvitationDecision() {
  const mounted = useSyncExternalStore(noop, () => true, () => false);
  return (
    <main className="mx-auto flex min-h-screen max-w-lg items-center p-6">
      <div className="w-full space-y-4 rounded-2xl border border-border bg-card p-6 shadow-sm">
        <h1 className="text-xl font-bold">Organization invitation</h1>
        {mounted ? <Decision /> : <p role="status">Loading…</p>}
      </div>
    </main>
  );
}

function Decision() {
  const [token] = useState(takeToken);
  const [signedIn] = useState(() => accountSession.hasSession());
  const decide = useInvitationDecision();
  const [done, setDone] = useState<null | { decision: "accept" | "decline"; organizationId: string | null }>(null);

  if (!token) {
    return (
      <p className="text-sm text-muted-foreground">
        This page needs the invitation link from your email. Open the link again, or ask the organization to send a new invitation.
      </p>
    );
  }

  if (!signedIn) {
    stash(token);
    return (
      <div className="space-y-3 text-sm">
        <p>Sign in with the email address the invitation was sent to, then come back here to accept or decline it.</p>
        <Button asChild>
          <Link href="/login?next=/organizations/invitations">Sign in</Link>
        </Button>
      </div>
    );
  }

  if (done) {
    return done.decision === "accept" ? (
      <div className="space-y-3 text-sm">
        <p role="status">You joined the organization.</p>
        <Button asChild>
          <Link href={done.organizationId ? `/dashboard/organizations/${encodeURIComponent(done.organizationId)}` : "/dashboard/organizations"}>
            Open organization
          </Link>
        </Button>
      </div>
    ) : (
      <div className="space-y-3 text-sm">
        <p role="status">You declined the invitation.</p>
        <Link className="underline" href="/dashboard">
          Go to your dashboard
        </Link>
      </div>
    );
  }

  const run = async (decision: "accept" | "decline") => {
    try {
      const result = await decide.mutateAsync({ token, decision });
      forget();
      const organizationId = typeof result?.organizationId === "string" ? result.organizationId : null;
      setDone({ decision, organizationId });
    } catch (error) {
      // A token that can never succeed is dropped; anything else may be retried.
      if (["INVITE_EXPIRED", "INVITATION_INVALID"].includes(errorEnvelopeOf(error).code)) forget();
    }
  };

  const code = decide.isError ? errorEnvelopeOf(decide.error).code : null;
  const final = code === "INVITE_EXPIRED" || code === "INVITATION_INVALID";

  return (
    <div className="space-y-3 text-sm">
      <p>You were invited to join an organization on CF Meet. Only the account whose verified email received the invitation can accept it.</p>
      {decide.isError && (
        <p role="alert" className="rounded-md border border-destructive/30 p-2 text-destructive">
          {code === "PERMISSION_DENIED"
            ? "This invitation was sent to a different email address than the one verified on your account."
            : organizationErrorText(decide.error)}
        </p>
      )}
      {!final && (
        <div className="flex gap-2">
          <Button disabled={decide.isPending} onClick={() => void run("accept")}>
            Accept
          </Button>
          <Button variant="outline" disabled={decide.isPending} onClick={() => void run("decline")}>
            Decline
          </Button>
        </div>
      )}
    </div>
  );
}
