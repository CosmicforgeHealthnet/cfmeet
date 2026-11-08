"use client";

import { Button } from "@/components/ui/button";
import { useOrganizationAudit } from "@/hooks/v2/useBusinessOrganizations";
import { shortId } from "@/lib/organizations/organization-core";
import { OrgError, Section, formatDateTime } from "./shared";

/** `GET /organizations/{id}/audit-events` (`organization.audit.read`), newest first, `before` paging. */
export default function OrganizationAudit({ organizationId }: { organizationId: string }) {
  const audit = useOrganizationAudit(organizationId, true);
  const events = audit.data?.pages.flatMap((p) => p.items ?? []) ?? [];

  return (
    <Section title="Audit log" description="Who changed what in this organization, and when.">
      {audit.isLoading && <p role="status" className="text-sm">Loading audit events…</p>}
      {audit.isError && <OrgError error={audit.error} retry={() => void audit.refetch()} />}
      {audit.data && events.length === 0 && <p className="text-sm text-muted-foreground">No audit events yet.</p>}
      {events.length > 0 && (
        <ul className="divide-y divide-border rounded-md border border-border text-sm">
          {events.map((e) => (
            <li key={e.id} className="grid gap-0.5 p-3">
              <span className="font-medium">{e.action}</span>
              <span className="text-muted-foreground">
                {formatDateTime(e.occurredAt)} · by{" "}
                {e.actor?.userId ? `account ${shortId(e.actor.userId)}` : e.actor?.serviceId ? `service ${e.actor.serviceId}` : e.actor?.kind ?? "system"}
                {e.targetType ? ` · ${e.targetType}${e.targetId ? ` ${shortId(e.targetId)}` : ""}` : ""}
              </span>
            </li>
          ))}
        </ul>
      )}
      {audit.hasNextPage && (
        <Button size="sm" variant="outline" disabled={audit.isFetchingNextPage} onClick={() => void audit.fetchNextPage()}>
          {audit.isFetchingNextPage ? "Loading…" : "Load older events"}
        </Button>
      )}
    </Section>
  );
}
