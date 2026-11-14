"use client";

import { useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { schedulingError } from "@/lib/meeting-scheduling";

export function Field({ label, className, ...props }: React.ComponentProps<typeof Input> & { label: string }) {
  return <Label className="grid gap-2 text-sm font-medium text-foreground"><span>{label}</span><Input {...props} className={`h-11 rounded-xl px-3 py-0 text-sm sm:h-11 sm:px-3 sm:py-0 sm:text-sm ${className ?? ""}`} /></Label>;
}

export function RequestError({ error, retry }: { error: unknown; retry?: () => void }) {
  return <div role="alert" className="flex flex-wrap items-center gap-3 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"><span className="flex-1">{schedulingError(error)}</span>{retry && <Button className="h-8 rounded-lg" variant="outline" size="sm" onClick={retry}>Try again</Button>}</div>;
}

export function ActionDialog({ label, title, description, children, submit, destructive = false, onOpen, disabled = false }: {
  label: string; title?: string; description: string; children?: ReactNode;
  submit: () => Promise<unknown>; destructive?: boolean;
  onOpen?: () => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>();
  return <Dialog open={open} onOpenChange={(value) => { if (!pending) { if (value) onOpen?.(); setOpen(value); setError(undefined); } }}>
    <DialogTrigger asChild><button type="button" disabled={disabled} className={destructive ? "inline-flex h-9 items-center rounded-xl border border-destructive/30 px-3.5 text-sm font-semibold text-destructive transition-colors hover:bg-destructive/10 disabled:opacity-50" : "inline-flex h-9 items-center rounded-xl border border-border bg-card px-3.5 text-sm font-semibold text-foreground transition-colors hover:bg-muted disabled:opacity-50"}>{label}</button></DialogTrigger>
    <DialogContent><form className="space-y-4" onSubmit={async (event) => {
      event.preventDefault(); setPending(true); setError(undefined);
      try { await submit(); setOpen(false); } catch (cause) { setError(cause); } finally { setPending(false); }
    }}>
      <DialogHeader><DialogTitle>{title || label}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader>
      <fieldset disabled={pending} className="space-y-4">{children}</fieldset>
      {error !== undefined && <RequestError error={error} />}
      <DialogFooter className="gap-2 sm:gap-2"><Button type="button" variant="outline" className="h-10 rounded-xl" disabled={pending} onClick={() => setOpen(false)}>Cancel</Button><Button type="submit" className="h-10 rounded-xl shadow-none" variant={destructive ? "destructive" : "default"} disabled={pending}>{pending ? "Saving…" : label}</Button></DialogFooter>
    </form></DialogContent>
  </Dialog>;
}
