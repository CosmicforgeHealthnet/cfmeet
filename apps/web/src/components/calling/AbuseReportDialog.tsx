"use client";

/**
 * AbuseReportDialog — report an unwanted or abusive call (CAL-005).
 *
 * POST /v1/calls/{id}/abuse-reports through the adapter, which injects the reporter. Calling
 * accepts a bounded category only (no free text: it never stores narrative, numbers or media)
 * and answers a repeat with the existing report.
 */

import { useState } from "react";
import { AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useReportAbuse } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { AbuseCategory } from "@/types/calling";

const categoryLabels: Record<AbuseCategory, string> = {
  UNWANTED: "Unwanted call",
  HARASSMENT: "Harassment",
  SPAM: "Spam",
  FRAUD_SUSPECTED: "Suspected fraud or scam",
  OTHER: "Something else",
};

export default function AbuseReportDialog({ callId, open, onOpenChange }: {
  callId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [category, setCategory] = useState<AbuseCategory>("UNWANTED");
  const [error, setError] = useState<string | null>(null);
  const reportAbuse = useReportAbuse(callId);
  const close = () => { onOpenChange(false); setError(null); reportAbuse.reset(); };

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2"><AlertTriangle className="size-5 text-destructive" /> Report this call</DialogTitle>
          <DialogDescription>
            Reports go to the CF Meet safety team. Only the category is sent; the call&apos;s audio is never shared. You can also block the caller in Call settings.
          </DialogDescription>
        </DialogHeader>
        {reportAbuse.isSuccess ? (
          <div className="space-y-2 py-4 text-center" role="status">
            <p className="font-medium">Report received</p>
            <p className="text-sm text-muted-foreground">Status: {reportAbuse.data.status.replaceAll("_", " ").toLowerCase()}. Reporting the same call again keeps your first report.</p>
            <DialogFooter><Button onClick={close}>Close</Button></DialogFooter>
          </div>
        ) : (
          <form className="space-y-4" onSubmit={(event) => {
            event.preventDefault();
            setError(null);
            reportAbuse.mutate({ category }, { onError: (e) => setError(callingErrorMessage(e)) });
          }}>
            {error ? <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error}</p> : null}
            <div className="space-y-2">
              <Label htmlFor="abuse-category">Reason</Label>
              <select id="abuse-category" value={category} onChange={(e) => setCategory(e.target.value as AbuseCategory)}
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm">
                {Object.entries(categoryLabels).map(([key, label]) => <option key={key} value={key}>{label}</option>)}
              </select>
            </div>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={close}>Cancel</Button>
              <Button type="submit" variant="destructive" disabled={reportAbuse.isPending}>
                {reportAbuse.isPending ? <Spinner className="mr-2 size-4" /> : null} Submit report
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
