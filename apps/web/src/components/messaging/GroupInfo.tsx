"use client";

/**
 * A synced group's photo and description (WhatsApp group info). The photo is a picture
 * uploaded to the group itself (scanned like any photo) and named as its avatar; members see
 * it in the chat list, the chat header and here. Owners and group admins change both.
 */
import { useRef, useState } from "react";
import { Camera, Pencil, Trash2 } from "lucide-react";
import type { ConversationView, MessagingEngine } from "@/lib/messaging/engine";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { Spinner } from "@/components/ui/spinner";
import { Avatar, useGroupPhoto } from "./ChatListRow";

export const MAX_GROUP_DESCRIPTION = 512;

/** The big photo of Group info, with a camera button for owners and admins. */
export function GroupPhoto({ engine, conversation, title, canEdit }: { engine: MessagingEngine; conversation: ConversationView; title: string; canEdit: boolean }) {
  const photo = useGroupPhoto(engine, conversation.id, conversation.avatarRef);
  const input = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const change = (file: File | null) => {
    setBusy(true);
    setError(null);
    engine.setGroupPhoto(conversation.id, file).catch((e) => setError(messagingErrorMessage(e))).finally(() => setBusy(false));
  };
  return (
    <div className="flex flex-col items-center" data-testid="group-photo">
      <span className="relative">
        <Avatar name={title} seed={conversation.id} size="size-28" className="text-4xl" photo={photo} />
        {busy ? <span className="absolute inset-0 flex items-center justify-center rounded-full bg-black/40"><Spinner /></span> : null}
        {canEdit ? (
          <button type="button" aria-label={photo ? "Change group photo" : "Add group photo"} title={photo ? "Change group photo" : "Add group photo"} disabled={busy} onClick={() => input.current?.click()} data-testid="group-photo-change"
            className="absolute -bottom-1 -right-1 flex size-10 items-center justify-center rounded-full border-4 border-background bg-primary text-primary-foreground shadow hover:bg-primary/90 disabled:opacity-60">
            <Camera className="size-4" />
          </button>
        ) : null}
      </span>
      {canEdit ? <input ref={input} type="file" accept="image/png,image/jpeg,image/webp,image/gif" hidden onChange={(e) => { const f = e.target.files?.[0]; e.target.value = ""; if (f) change(f); }} /> : null}
      {canEdit && conversation.avatarRef ? (
        <button type="button" disabled={busy} onClick={() => change(null)} className="mt-2 inline-flex items-center gap-1 rounded-full px-2.5 py-1 text-xs font-medium text-muted-foreground hover:bg-muted hover:text-destructive"><Trash2 className="size-3.5" />Remove photo</button>
      ) : null}
      {error ? <p role="alert" className="mt-1 text-xs text-destructive">{error}</p> : null}
    </div>
  );
}

/** The group's description: read by everyone, written by owners and admins (512 characters). */
export function GroupDescription({ engine, conversation, canEdit }: { engine: MessagingEngine; conversation: ConversationView; canEdit: boolean }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(conversation.description ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!conversation.description && !canEdit) return null;
  const save = () => {
    setBusy(true);
    setError(null);
    engine.setGroupDescription(conversation.id, text.trim() || null)
      .then(() => setEditing(false), (e) => setError(messagingErrorMessage(e)))
      .finally(() => setBusy(false));
  };
  if (editing) {
    return (
      <form className="w-full space-y-2 text-left" onSubmit={(e) => { e.preventDefault(); save(); }} data-testid="group-description-form">
        <textarea autoFocus aria-label="Group description" placeholder="Add a group description" maxLength={MAX_GROUP_DESCRIPTION} rows={4} value={text} onChange={(e) => setText(e.target.value)}
          className="w-full resize-none rounded-xl border border-border bg-background px-3 py-2 text-sm outline-none focus:border-primary" />
        <div className="flex items-center justify-between gap-2">
          <span className="text-xs tabular-nums text-muted-foreground">{MAX_GROUP_DESCRIPTION - text.length}</span>
          <span className="flex gap-1">
            <button type="button" onClick={() => { setEditing(false); setText(conversation.description ?? ""); setError(null); }} className="h-9 rounded-xl px-3 text-sm font-medium hover:bg-muted">Cancel</button>
            <button type="submit" disabled={busy} data-testid="group-description-save" className="h-9 rounded-xl bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">Save</button>
          </span>
        </div>
        {error ? <p role="alert" className="text-xs text-destructive">{error}</p> : null}
      </form>
    );
  }
  return (
    <div className="w-full text-left" data-testid="group-description">
      {conversation.description ? (
        <div className="flex items-start gap-2 rounded-xl px-3 py-2 hover:bg-muted/50">
          <p className="min-w-0 flex-1 whitespace-pre-line break-words text-sm">{conversation.description}</p>
          {canEdit ? <button type="button" aria-label="Edit group description" onClick={() => { setText(conversation.description ?? ""); setEditing(true); }} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Pencil className="size-4" /></button> : null}
        </div>
      ) : (
        <button type="button" onClick={() => setEditing(true)} data-testid="group-description-add" className="w-full rounded-xl px-3 py-2 text-center text-sm font-medium text-primary hover:bg-primary/10">Add group description</button>
      )}
    </div>
  );
}
