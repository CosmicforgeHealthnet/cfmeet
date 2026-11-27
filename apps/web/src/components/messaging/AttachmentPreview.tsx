"use client";

/**
 * The file you picked, before it goes (WhatsApp): a photo or video shows full size, any other
 * file shows its name and size. Nothing is sent until you press Send.
 */
import { useEffect, useMemo, useState } from "react";
import { FileText, SendHorizontal, X } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { isInlineImage } from "@/lib/messaging/engine";

const size = (bytes: number) =>
  bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${Math.round(bytes / 1024)} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`;

export function AttachmentPreview({ file, to, onSend, onCancel }: { file: File | null; to: string; onSend: (file: File, caption: string) => void; onCancel: () => void }) {
  const [caption, setCaption] = useState("");
  const [forFile, setForFile] = useState<File | null>(file);
  // A new file starts with an empty caption.
  if (forFile !== file) { setForFile(file); setCaption(""); }
  const send = () => { if (file) onSend(file, caption); };
  const photo = !!file && isInlineImage(file.type);
  const video = !!file && /^video\/(mp4|webm|ogg|quicktime)$/.test(file.type);
  const url = useMemo(() => (file && (photo || video) ? URL.createObjectURL(file) : null), [file, photo, video]);
  useEffect(() => () => { if (url) URL.revokeObjectURL(url); }, [url]);

  return (
    <Dialog open={!!file} onOpenChange={(open) => { if (!open) onCancel(); }}>
      <DialogContent showCloseButton={false} className="flex max-h-[92dvh] w-[calc(100vw-2rem)] max-w-xl flex-col gap-0 overflow-hidden p-0" data-testid="attachment-preview">
        <div className="flex items-center gap-2 border-b border-border px-3 py-2">
          <button type="button" aria-label="Cancel" onClick={onCancel} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><X className="size-5" /></button>
          <span className="min-w-0 flex-1">
            <DialogTitle className="truncate text-sm font-semibold">{file?.name}</DialogTitle>
            <DialogDescription className="text-xs">{file ? `${size(file.size)} · to ${to}` : ""}</DialogDescription>
          </span>
        </div>
        <div className="flex min-h-56 flex-1 items-center justify-center overflow-hidden bg-muted/50 p-3">
          {photo && url ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img src={url} alt="" className="max-h-[65dvh] max-w-full rounded-lg object-contain" />
          ) : video && url ? (
            <video src={url} controls className="max-h-[65dvh] max-w-full rounded-lg" />
          ) : file ? (
            <span className="flex flex-col items-center gap-3 text-center">
              <span className="flex size-16 items-center justify-center rounded-2xl bg-primary/10 text-primary"><FileText className="size-8" /></span>
              <span className="max-w-xs break-all text-sm font-medium">{file.name}</span>
              <span className="text-xs text-muted-foreground">{size(file.size)}</span>
            </span>
          ) : null}
        </div>
        <form className="flex items-end gap-2 border-t border-border p-3" onSubmit={(e) => { e.preventDefault(); send(); }}>
          <textarea autoFocus rows={1} value={caption} maxLength={1024} placeholder="Add a caption" aria-label="Caption" data-testid="attachment-caption"
            onChange={(e) => setCaption(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); } }}
            className="max-h-32 min-h-11 flex-1 resize-none rounded-3xl border border-border bg-background px-4 py-2.5 text-sm outline-none [field-sizing:content] focus:border-primary/50" />
          <button type="submit" aria-label="Send" data-testid="attachment-send"
            className="flex size-11 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground hover:bg-primary/90">
            <SendHorizontal className="size-5" />
          </button>
        </form>
      </DialogContent>
    </Dialog>
  );
}
