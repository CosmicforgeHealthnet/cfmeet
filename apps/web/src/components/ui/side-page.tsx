"use client";

import * as React from "react";
import { ArrowLeft, X } from "lucide-react";
import { Dialog as DialogPrimitive } from "radix-ui";
import { cn } from "@/lib/utils";

/**
 * A screen that slides in (WhatsApp "Contact info" / "New contact" pattern): full screen on
 * phones with a back arrow, a panel on the right on larger screens with a close button.
 */
export function SidePage({ open, onOpenChange, title, description, children, className }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 hidden bg-black/30 data-[state=open]:animate-in data-[state=open]:fade-in-0 md:block" />
        <DialogPrimitive.Content
          className={cn(
            "fixed inset-0 z-50 flex flex-col overflow-hidden bg-background outline-none data-[state=open]:animate-in data-[state=open]:slide-in-from-right-8 md:left-auto md:w-[26rem] md:border-l md:border-border md:shadow-2xl",
            className,
          )}
        >
          <header className="flex h-16 shrink-0 items-center gap-2 border-b border-border px-2">
            <DialogPrimitive.Close className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground" aria-label="Back">
              <ArrowLeft className="size-5 md:hidden" />
              <X className="hidden size-5 md:block" />
            </DialogPrimitive.Close>
            <DialogPrimitive.Title className="text-lg font-semibold">{title}</DialogPrimitive.Title>
            {description ? <DialogPrimitive.Description className="sr-only">{description}</DialogPrimitive.Description> : null}
          </header>
          <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto overflow-x-hidden">{children}</div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

/** A grouped list section (WhatsApp settings rows): optional heading, rows in one rounded card. */
export function SideSection({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <section className="px-4 pt-4">
      {title ? <p className="px-2 pb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{title}</p> : null}
      <div className="overflow-hidden rounded-2xl bg-muted/40 ring-1 ring-border">{children}</div>
    </section>
  );
}

/** One row inside a SideSection. */
export function SideRow({ icon, label, hint, onClick, danger, trailing, disabled }: {
  icon: React.ReactNode;
  label: React.ReactNode;
  hint?: React.ReactNode;
  onClick?: () => void;
  danger?: boolean;
  trailing?: React.ReactNode;
  disabled?: boolean;
}) {
  const body = (
    <>
      <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-full [&_svg]:size-[18px]", danger ? "text-destructive" : "text-muted-foreground")}>{icon}</span>
      <span className="min-w-0 flex-1">
        <span className={cn("block truncate text-[15px]", danger && "font-medium text-destructive")}>{label}</span>
        {hint ? <span className="block truncate text-xs text-muted-foreground">{hint}</span> : null}
      </span>
      {trailing}
    </>
  );
  const cls = "flex w-full items-center gap-3 px-3 py-3 text-left border-t border-border first:border-t-0";
  return onClick
    ? <button type="button" onClick={onClick} disabled={disabled} className={cn(cls, "hover:bg-muted/70 disabled:opacity-50")}>{body}</button>
    : <div className={cls}>{body}</div>;
}

/** On/off switch for SideRow trailing. */
export function Toggle({ on }: { on: boolean }) {
  return (
    <span aria-hidden className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", on ? "bg-primary" : "bg-muted-foreground/30")}>
      <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", on ? "left-[1.125rem]" : "left-0.5")} />
    </span>
  );
}
