"use client";

import { useState } from "react";
import Image from "next/image";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { linkPreviewFor, type RichPayload } from "@/lib/messaging/rich-content";

async function thumbnail(file: File): Promise<string> {
  if (file.size > 5 * 1024 * 1024 || !["image/jpeg", "image/png", "image/webp"].includes(file.type)) throw new Error("Choose a JPEG, PNG or WebP image under 5 MB.");
  const bitmap = await createImageBitmap(file);
  try {
    if (bitmap.width * bitmap.height > 40_000_000) throw new Error("This image is too large.");
    const canvas = document.createElement("canvas");
    canvas.width = 256; canvas.height = 144;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("This browser cannot prepare a thumbnail.");
    context.fillStyle = "#ffffff"; context.fillRect(0, 0, 256, 144);
    const scale = Math.min(256 / bitmap.width, 144 / bitmap.height);
    const width = bitmap.width * scale, height = bitmap.height * scale;
    context.drawImage(bitmap, (256 - width) / 2, (144 - height) / 2, width, height);
    const result = canvas.toDataURL("image/jpeg", 0.55);
    if (result.length > 24576) throw new Error("Choose a simpler image for this thumbnail.");
    return result;
  } finally { bitmap.close(); }
}

export default function LinkPreviewDialog({ onClose, onSend }: { onClose: () => void; onSend: (payload: RichPayload) => Promise<void> }) {
  const [url, setUrl] = useState("");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [image, setImage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const preview = linkPreviewFor(url);
  const send = async () => {
    if (!preview || busy) return;
    setBusy(true); setError(null);
    try { await onSend({ t: "text", text: preview.url, preview: { ...preview, title: title.trim() || null, description: description.trim() || null, ...(image ? { imageDataUrl: image } : {}) } }); onClose(); }
    catch { setError("Sending was not confirmed. Check the chat before trying again."); }
    finally { setBusy(false); }
  };
  const field = "h-10 w-full rounded-xl border bg-background px-3 text-sm";
  return <Dialog open onOpenChange={(open) => { if (!open && !busy) onClose(); }}><DialogContent>
    <DialogHeader><DialogTitle>Share a link card</DialogTitle><DialogDescription>Add a title, description and optional image. The card is encrypted with your message. Images are prepared on your device.</DialogDescription></DialogHeader>
    <form className="space-y-3" onSubmit={(event) => { event.preventDefault(); void send(); }}>
      <label className="block text-sm">Link<input required type="url" className={field} value={url} maxLength={2048} onChange={(event) => setUrl(event.target.value)} /></label>
      <label className="block text-sm">Title<input className={field} value={title} maxLength={200} onChange={(event) => setTitle(event.target.value)} /></label>
      <label className="block text-sm">Description<input className={field} value={description} maxLength={300} onChange={(event) => setDescription(event.target.value)} /></label>
      <label className="block text-sm">Thumbnail<input type="file" disabled={busy} accept="image/jpeg,image/png,image/webp" className="block w-full text-sm" onChange={async (event) => {
        const file = event.target.files?.[0]; if (!file) return;
        setBusy(true); setError(null);
        try { setImage(await thumbnail(file)); } catch (cause) { setError(cause instanceof Error ? cause.message : "Unable to prepare image"); } finally { setBusy(false); }
      }} /></label>
      {image ? <div className="flex items-center gap-3"><Image src={image} alt="Link thumbnail" width={128} height={72} unoptimized /><button type="button" onClick={() => setImage(null)} disabled={busy}>Remove image</button></div> : null}
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      <button type="submit" disabled={busy || !preview} className="h-11 w-full rounded-xl bg-primary text-primary-foreground disabled:opacity-50">{busy ? "Preparing…" : "Send link card"}</button>
    </form>
  </DialogContent></Dialog>;
}
