"use client";

import { useState } from "react";
import { toast } from "@/components/ui/toast";
import { REPORT_CATEGORIES } from "@/lib/channels";
import { channelsService } from "@/services/v2/channels.service";
import { errorText } from "@/components/updates/shared";
import { Sheet } from "./bits";

/** Report a channel or one of its posts. CF Meet reviews reports; the note stays private to reviewers. */
export default function ReportDialog({ channelId, postId, onClose }: { channelId: string; postId?: string; onClose: () => void }) {
  const [category, setCategory] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const send = async () => {
    if (!category || busy) return;
    setBusy(true);
    try {
      await channelsService.report(channelId, { category, postId, note: note.trim() || undefined });
      toast("Thanks — CF Meet will review your report.");
      onClose();
    } catch (e) {
      toast(errorText(e, "Your report couldn't be sent."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet title={postId ? "Report post" : "Report channel"} onClose={onClose}>
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); void send(); }} data-testid="report-dialog">
        <fieldset className="space-y-1">
          <legend className="mb-1 text-sm text-muted-foreground">What&apos;s wrong?</legend>
          {REPORT_CATEGORIES.map((c) => (
            <label key={c.code} className="flex items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted">
              <input type="radio" name="category" value={c.code} checked={category === c.code} onChange={() => setCategory(c.code)} className="accent-primary" />
              <span className="text-sm">{c.label}</span>
            </label>
          ))}
        </fieldset>
        <textarea value={note} onChange={(e) => setNote(e.target.value)} maxLength={500} rows={3} placeholder="Add details (optional)"
          className="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-primary/40" />
        <p className="text-xs text-muted-foreground">The channel&apos;s admins aren&apos;t told who reported it.</p>
        <button type="submit" disabled={!category || busy} className="h-11 w-full rounded-xl bg-destructive font-semibold text-white disabled:opacity-50">
          {busy ? "Sending…" : "Report"}
        </button>
      </form>
    </Sheet>
  );
}
