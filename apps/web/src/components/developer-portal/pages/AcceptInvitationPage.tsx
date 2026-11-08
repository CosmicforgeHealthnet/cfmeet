"use client";

import { useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { PORTAL_BASE } from "@/lib/developer-platform/navigation";
import type { Member } from "@/lib/developer-platform/types";
import { useDeveloperPortal, WORKSPACES_QUERY_KEY } from "../DeveloperPortalProvider";
import { ErrorNotice, Field, inputClass, PageHeader, Section, useConsoleMutation } from "../ui";

/**
 * Accept a developer-workspace invitation with the token the owner handed over. The token
 * is typed or pasted (never read from the URL, so it does not land in history or logs).
 */
export default function AcceptInvitationPage() {
  const { client, selectWorkspace } = useDeveloperPortal();
  const [token, setToken] = useState("");
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
  );
  return (
    <div className="space-y-6">
      <PageHeader
        title="Accept an invitation"
        description="Paste the invitation token a developer workspace owner sent you. You must be signed in with the email address the invitation was issued to, and that address must be verified."
      />
      <Section title="Invitation token">
        {joined !== null ? (
          <div className="space-y-3 text-sm">
            <p>You joined the developer workspace.</p>
            <Button asChild size="sm">
              <Link href={PORTAL_BASE}>Open the workspace</Link>
            </Button>
          </div>
        ) : (
          <form
            className="space-y-3"
            autoComplete="off"
            onSubmit={(e) => {
              e.preventDefault();
              if (token.trim()) accept.mutate(undefined);
            }}
          >
            <Field label="Token">
              <input className={`${inputClass} font-mono`} value={token} spellCheck={false} onChange={(e) => setToken(e.target.value)} />
            </Field>
            <Button type="submit" size="sm" loading={accept.isPending} disabled={!token.trim()}>
              Accept invitation
            </Button>
            <ErrorNotice error={accept.error} />
          </form>
        )}
      </Section>
    </div>
  );
}
