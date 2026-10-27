"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useArchiveSpace, useDuplicateSpace, useTransferOwnership } from "@/hooks/v2/useSpaces";
import { spacePermissions } from "@/lib/meeting-spaces";
import { getErrorMessage } from "@/utils/error";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { MeetingSpaceResponse } from "@/types/v2";

export default function MeetingSpaceActions({ space }: { space: MeetingSpaceResponse }) {
  const router = useRouter();
  const permissions = spacePermissions(space);
  const archive = useArchiveSpace();
  const duplicate = useDuplicateSpace();
  const transfer = useTransferOwnership(space.id);
  const [action, setAction] = useState<"archive" | "duplicate" | "transfer" | null>(null);
  const [reviewedVersion, setReviewedVersion] = useState<number | undefined>();
  const [title, setTitle] = useState("");
  const [newOwner, setNewOwner] = useState("");
  const [retain, setRetain] = useState(true);
  const [error, setError] = useState("");
  const pending = archive.isPending || duplicate.isPending || transfer.isPending;
  const open = (next: typeof action) => { setError(""); setReviewedVersion(space.version); setTitle(`Copy of ${space.title ?? "meeting space"}`.slice(0, 255)); setNewOwner(""); setRetain(true); setAction(next); };
  if (!permissions.configure) return null;
  return <>
    <div className="flex flex-wrap items-center gap-1">
      {permissions.active ? <Button variant="ghost" size="sm" onClick={() => router.push(`/dashboard/meetings/${space.id}/edit`)}>Edit</Button> : null}
      <Button variant="ghost" size="sm" onClick={() => open("duplicate")}>Duplicate</Button>
      {permissions.owner && permissions.active && space.version !== undefined ? <>
        <Button variant="ghost" size="sm" onClick={() => open("transfer")}>Transfer owner</Button>
        <Button variant="ghost" size="sm" onClick={() => open("archive")}>Archive</Button>
      </> : null}
    </div>
    <Dialog open={action !== null} onOpenChange={(value) => { if (!value && !pending) setAction(null); }}>
      <DialogContent><form className="space-y-4" onSubmit={async (event) => {
        event.preventDefault(); setError("");
        try {
          if (action === "duplicate") {
            const created = await duplicate.mutateAsync({ id: space.id, data: { title: title.trim() } });
            setAction(null); router.push(`/dashboard/meetings/${created.id}/edit`); return;
          }
          if (reviewedVersion === undefined || reviewedVersion !== space.version) throw new Error("This meeting space changed. Close this dialog and review its latest settings before retrying.");
          if (action === "archive") await archive.mutateAsync({ id: space.id, version: reviewedVersion });
          if (action === "transfer") await transfer.mutateAsync({ newOwnerUserId: newOwner.trim(), version: reviewedVersion, retainPreviousOwner: retain });
          setAction(null);
        } catch (cause) { setError(getErrorMessage(cause, "Could not update the meeting space.")); }
      }}>
        <DialogHeader><DialogTitle>{action === "archive" ? "Archive meeting space?" : action === "duplicate" ? "Duplicate meeting space" : "Transfer ownership?"}</DialogTitle><DialogDescription>
          {action === "archive" ? "History is preserved. Recurrence is stopped and unstarted future occurrences are cancelled. This does not end an ongoing conference." : action === "duplicate" ? "Copy reusable settings into a new space owned by you. This does not create a live conference." : "The new owner must be an active registered CF Meet user. Ownership changes only through this protected operation."}
        </DialogDescription></DialogHeader>
        {action === "duplicate" ? <div className="space-y-2"><Label htmlFor={`duplicate-${space.id}`}>New space title</Label><Input id={`duplicate-${space.id}`} value={title} onChange={(event) => setTitle(event.target.value)} maxLength={255} required /></div> : null}
        {action === "transfer" ? <>
          <div className="space-y-2"><Label htmlFor={`owner-${space.id}`}>New owner’s account ID</Label><Input id={`owner-${space.id}`} value={newOwner} onChange={(event) => setNewOwner(event.target.value)} pattern="[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}" required placeholder="User UUID" /></div>
          <label className="flex items-center gap-2 text-sm"><Checkbox checked={retain} onCheckedChange={(checked) => setRetain(checked === true)} /> Keep me as a host member</label>
        </> : null}
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
        <DialogFooter><Button type="button" variant="outline" disabled={pending} onClick={() => setAction(null)}>Cancel</Button><Button type="submit" disabled={pending} variant={action === "duplicate" ? "default" : "destructive"}>{pending ? "Saving…" : action === "archive" ? "Archive space" : action === "duplicate" ? "Create copy" : "Transfer ownership"}</Button></DialogFooter>
      </form></DialogContent>
    </Dialog>
  </>;
}
