"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { ArrowRight, CheckCircle2, Mail } from "lucide-react";
import { Button } from "@/components/ui/button";
import { PORTAL_BASE } from "@/lib/developer-platform/navigation";
import type { Member } from "@/lib/developer-platform/types";
import { useDeveloperPortal, WORKSPACES_QUERY_KEY } from "../DeveloperPortalProvider";
import { ErrorNotice, inputClass, useConsoleMutation } from "../ui";

/**
 * Accept a developer-workspace invitation with the token the owner handed over. The token
 * is typed or pasted, or arrives in the single-use claim link CF Meet emails to a
 * first-party workspace owner (ADR 0068 A4): only in the URL fragment, which never reaches
 * a server or its logs, and which is removed from the address bar (and the history entry)
 * as soon as the page reads it.
 */
export default function AcceptInvitationPage() {
  const { client, selectWorkspace } = useDeveloperPortal();
  const [token, setToken] = useState("");
  useEffect(() => {
    const fragment = new URLSearchParams(window.location.hash.replace(/^#/, ""));
    const claimed = fragment.get("token");
    if (claimed) {
      window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search);
      // The fragment exists only in the browser (read once after mount, never during SSR).
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setToken(claimed);
    }
  }, []);
  const [joined, setJoined] = useState<string | null>(null);
  const accept = useConsoleMutation(
    () => client.call<Member>({ operation: "acceptInvitation", body: { token: token.trim() } }),
    [WORKSPACES_QUERY_KEY],
    (member) => {
      setToken("");
      if (member?.organizationId) {
        selectWorkspace(member.organizationId);
        setJoined(member.organizationId);
      } else {
        setJoined("");
      }
    },
    "",
  );
  return (
    <div className="mx-auto max-w-lg py-6">
      <div className="rounded-3xl border border-border bg-card p-6 sm:p-8">
        {joined !== null ? (
          <div className="text-center">
            <span className="mx-auto flex size-16 items-center justify-center rounded-2xl bg-emerald-500/10 text-emerald-600"><CheckCircle2 className="size-8" /></span>
            <h1 className="mt-5 text-2xl font-bold tracking-tight">You&apos;re in</h1>
            <p className="mt-2 text-sm text-muted-foreground">You joined the workspace. Your role decides what you can do there.</p>
            <Button asChild className="mt-6 rounded-full px-6"><Link href={PORTAL_BASE}>Open the workspace<ArrowRight className="size-4" /></Link></Button>
          </div>
        ) : (
          <>
            <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Mail className="size-6" /></span>
            <h1 className="mt-5 text-2xl font-bold tracking-tight">Join a workspace</h1>
            <p className="mt-2 text-sm leading-relaxed text-muted-foreground">Paste the invitation code a workspace owner sent you. Make sure you&apos;re signed in with the email they invited, and that it&apos;s verified.</p>
            <form className="mt-6 space-y-4" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (token.trim()) accept.mutate(undefined); }}>
              <textarea aria-label="Invitation code" autoFocus rows={3} className={`${inputClass} h-auto resize-none py-3 font-mono`} placeholder="Paste the code here" value={token} spellCheck={false} onChange={(e) => setToken(e.target.value)} />
              <ErrorNotice error={accept.error} />
              <Button type="submit" className="h-11 w-full rounded-full" loading={accept.isPending} disabled={!token.trim()}>Join workspace</Button>
            </form>
          </>
        )}
      </div>
    </div>
  );
}
