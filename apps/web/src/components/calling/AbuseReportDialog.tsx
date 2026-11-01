"use client";

/**
 * AbuseReportDialog - Report an unwanted or abusive call.
 */

import { useState } from "react";
import { AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useReportAbuse } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { AbuseCategory } from "@/types/calling";

interface AbuseReportDialogProps {
  callId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const categoryLabels: Record<AbuseCategory, string> = {
  HARASSMENT: "Harassment",
  SPAM: "Spam",
  SCAM: "Scam / Fraud",
  THREAT: "Threat / Violence",
  INAPPROPRIATE_CONTENT: "Inappropriate Content",
  IMPERSONATION: "Impersonation",
  OTHER: "Other",
};

export default function AbuseReportDialog({
  callId,
  open,
  onOpenChange,
}: AbuseReportDialogProps) {
  const [category, setCategory] = useState<AbuseCategory>("SPAM");
  const [description, setDescription] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const reportAbuse = useReportAbuse(callId);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await reportAbuse.mutateAsync({
        category,
        description: description.trim() || undefined,
      });
      setSuccess(true);
      setTimeout(() => {
        onOpenChange(false);
        setSuccess(false);
        setDescription("");
      }, 2000);
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const handleClose = () => {
    onOpenChange(false);
    setError(null);
    setSuccess(false);
    setDescription("");
  };

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <AlertTriangle className="size-5 text-destructive" />
            Report Call
          </DialogTitle>
          <DialogDescription>
            Report this call if it was unwanted, abusive, or violated our terms.
            Your report helps us keep the platform safe.
          </DialogDescription>
        </DialogHeader>

        {success ? (
          <div className="py-6 text-center">
            <div className="mx-auto mb-4 flex size-12 items-center justify-center rounded-full bg-green-100">
              <svg className="size-6 text-green-600" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 13l4 4L19 7" />
              </svg>
            </div>
            <p className="font-medium">Report Submitted</p>
            <p className="text-sm text-muted-foreground">Thank you for your report.</p>
          </div>
        ) : (
          <form onSubmit={handleSubmit} className="space-y-4">
            {error && (
              <p className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
                {error}
              </p>
            )}

            <div className="space-y-2">
              <Label htmlFor="abuse-category">Reason for Report</Label>
              <select
                id="abuse-category"
                value={category}
                onChange={(e) => setCategory(e.target.value as AbuseCategory)}
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
              >
                {Object.entries(categoryLabels).map(([key, label]) => (
                  <option key={key} value={key}>{label}</option>
                ))}
              </select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="abuse-description">Additional Details (optional)</Label>
              <textarea
                id="abuse-description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="Please describe what happened..."
                className="min-h-[100px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              />
            </div>

            <DialogFooter>
              <Button type="button" variant="outline" onClick={handleClose}>
                Cancel
              </Button>
              <Button type="submit" variant="destructive" disabled={reportAbuse.isPending}>
                {reportAbuse.isPending && <Spinner className="mr-2 size-4" />}
                Submit Report
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
