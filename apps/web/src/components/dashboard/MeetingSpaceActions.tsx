"use client";

import { useEffect, useRef, useState } from "react";
import { Archive, Copy, MoreHorizontal, Pencil, UserRoundCog } from "lucide-react";
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

type MenuItem = { label: string; icon: React.ComponentType<{ className?: string }>; run: () => void; danger?: boolean };

export default function MeetingSpaceActions({ space, extraItems = [] }: { space: MeetingSpaceResponse; extraItems?: MenuItem[] }) {
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
  if (!permissions.configure && extraItems.length === 0) return null;
  const ownerActions = permissions.owner && permissions.active && space.version !== undefined;
  const items = [
    ...extraItems,
    !permissions.configure ? null : permissions.active ? { label: "Edit", icon: Pencil, run: () => router.push(`/dashboard/meetings/${space.id}/edit`) } : null,
    permissions.configure ? { label: "Duplicate", icon: Copy, run: () => open("duplicate") } : null,
    ownerActions ? { label: "Transfer owner", icon: UserRoundCog, run: () => open("transfer") } : null,
    ownerActions ? { label: "Archive", icon: Archive, run: () => open("archive"), danger: true } : null,
  ].filter((i): i is { label: string; icon: typeof Pencil; run: () => void; danger?: boolean } => i !== null);
  return <>
    <ActionsMenu label={`More actions for ${space.title || "meeting space"}`} items={items} />
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

/** "More" (⋯) menu: secondary space actions, kept out of the row so it stays clean. */
function ActionsMenu({ label, items }: { label: string; items: { label: string; icon: React.ComponentType<{ className?: string }>; run: () => void; danger?: boolean }[] }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex size-9 items-center justify-center rounded-xl border border-border text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      >
        <MoreHorizontal className="size-4" />
      </button>
      {open && (
        <div role="menu" className="absolute right-0 top-full z-40 mt-2 w-48 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
          {items.map(({ label: text, icon: Icon, run, danger }) => (
            <button
              key={text}
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                run();
              }}
              className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-sm transition-colors hover:bg-muted ${danger ? "text-destructive" : "text-foreground"}`}
            >
              <Icon className="size-4" />
              {text}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
