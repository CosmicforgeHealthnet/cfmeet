"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import {
  canClaim,
  deliveryStateText,
  downloadOutcome,
  downloadOutcomeText,
  humanSize,
  orderedDeliveries,
  safeDownloadUrl,
} from "@/lib/account/owner-exports";
import { followDownload } from "@/lib/account/save-file";
import { ownerExportsService } from "@/services/owner-exports.service";

const KEY = ["owner-exports", "messaging"] as const;

/**
 * "A data export is waiting for you" (ADR 0068): Messaging exports CF Meet support requested
 * for you. Claiming one returns a single short-lived download link, used straight away and
 * never kept; afterwards the export is marked downloaded.
 */
export default function MessagingExportDeliveries() {
  const queryClient = useQueryClient();
  const list = useQuery({ queryKey: KEY, queryFn: ownerExportsService.listMessagingExports, staleTime: 30_000 });
  const [message, setMessage] = useState<string | null>(null);
  const claim = useMutation({
    mutationFn: (exportId: string) => ownerExportsService.claimMessagingExport(exportId),
    onSuccess: (grant) => {
      const url = safeDownloadUrl(grant.url);
      if (url) {
        followDownload(url);
        setMessage("Your download started. This export can't be downloaded again.");
      } else {
        setMessage("The download link was not usable. Ask CF Meet support to request the export again.");
      }
    },
    onError: (error) => setMessage(downloadOutcomeText(downloadOutcome(error))),
    onSettled: () => queryClient.invalidateQueries({ queryKey: KEY }),
  });
  // Read once per render: the list re-renders when it refreshes.
  const [now] = useState(() => Date.now());
  const items = orderedDeliveries(list.data ?? [], now);

  return (
    <section className="mx-auto max-w-2xl space-y-4 p-6">
      <header className="space-y-1">
        <h1 className="text-2xl font-bold">Data exports waiting for you</h1>
        <p className="text-sm text-muted-foreground">
          Copies of your messages that CF Meet support requested for you. Only you can download each one, once. CF Meet support never sees them.
        </p>
      </header>
      {message && (
        <p role="status" className="rounded-md border border-border p-2 text-sm">
          {message}
        </p>
      )}
      {list.isPending ? (
        <p role="status">Loading…</p>
      ) : list.isError ? (
        <p role="alert" className="text-sm text-destructive">
          {downloadOutcomeText(downloadOutcome(list.error))}
        </p>
      ) : items.length === 0 ? (
        <p className="text-sm text-muted-foreground">No data export is waiting for you.</p>
      ) : (
        <ul className="divide-y divide-border rounded-xl border border-border">
          {items.map((d) => (
            <li key={d.exportId} className="flex items-center justify-between gap-3 p-4 text-sm">
              <div className="min-w-0">
                <p className="font-medium">Messages export · {new Date(d.createdAt).toLocaleDateString()}</p>
                <p className="text-muted-foreground">
                  {deliveryStateText(d, now)}
                  {d.size !== null ? ` · ${humanSize(d.size)}` : ""}
                  {canClaim(d, now) ? ` · until ${new Date(d.expiresAt).toLocaleString()}` : ""}
                </p>
              </div>
              {canClaim(d, now) && (
                <Button
                  disabled={claim.isPending}
                  onClick={() => {
                    setMessage(null);
                    claim.mutate(d.exportId);
                  }}
                >
                  Download
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
