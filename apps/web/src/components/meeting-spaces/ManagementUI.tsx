"use client";

import { useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { schedulingError } from "@/lib/meeting-scheduling";

export function Field({ label, ...props }: React.ComponentProps<typeof Input> & { label: string }) {
  return <Label className="grid gap-2"><span>{label}</span><Input {...props} /></Label>;
}

export function RequestError({ error, retry }: { error: unknown; retry?: () => void }) {
  return <div role="alert" className="rounded-lg border border-destructive/30 p-3 text-sm text-destructive">{schedulingError(error)}{retry && <Button className="ml-3" variant="outline" size="sm" onClick={retry}>Retry</Button>}</div>;
}

export function ActionDialog({ label, title, description, children, submit, destructive = false, onOpen }: {
  label: string; title?: string; description: string; children?: ReactNode;
  submit: () => Promise<unknown>; destructive?: boolean;
  onOpen?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>();
  return <Dialog open={open} onOpenChange={(value) => { if (!pending) { if (value) onOpen?.(); setOpen(value); setError(undefined); } }}>
    <DialogTrigger asChild><Button size="sm" variant={destructive ? "destructive" : "outline"}>{label}</Button></DialogTrigger>
    <DialogContent><form className="space-y-4" onSubmit={async (event) => {
      event.preventDefault(); setPending(true); setError(undefined);
      try { await submit(); setOpen(false); } catch (cause) { setError(cause); } finally { setPending(false); }
    }}>
      <DialogHeader><DialogTitle>{title || label}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader>
      <fieldset disabled={pending} className="space-y-4">{children}</fieldset>
      {error !== undefined && <RequestError error={error} />}
      <DialogFooter><Button type="button" variant="outline" disabled={pending} onClick={() => setOpen(false)}>Close</Button><Button type="submit" variant={destructive ? "destructive" : "default"} disabled={pending}>{pending ? "Saving…" : label}</Button></DialogFooter>
    </form></DialogContent>
  </Dialog>;
}
