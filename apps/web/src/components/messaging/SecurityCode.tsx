"use client";

/**
 * "Verify security code" (WhatsApp) for a private one-to-one chat: both people see the same
 * 60 digits and compare them in person. The key-log line says plainly who checks that log today
 * (CF Meet itself), so it never sounds like an independent audit.
 */
import { useEffect, useState } from "react";
import { ChevronRight, Lock, ShieldAlert, ShieldCheck } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { MessagingEngine } from "@/lib/messaging/engine";

export function SecurityCode({ engine, conversationId, name }: { engine: MessagingEngine; conversationId: string; name: string }) {
  const [open, setOpen] = useState(false);
  const [result, setResult] = useState<{ code: string; logChecked: boolean } | null | "error" | undefined>(undefined);

  useEffect(() => {
    if (!open) return;
    let active = true;
    engine.securityCode(conversationId).then((r) => { if (active) setResult(r); }, () => { if (active) setResult("error"); });
    return () => { active = false; };
  }, [open, engine, conversationId]);

  const code = result && result !== "error" ? result : null;
  return (
    <>
      <button type="button" onClick={() => { setResult(undefined); setOpen(true); }} data-testid="security-code"
        className="flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left hover:bg-muted">
        <Lock className="size-5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1"><span className="block text-sm font-medium">Encryption</span><span className="block text-xs text-muted-foreground">Messages are end-to-end encrypted. Tap to verify.</span></span>
        <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
      </button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>Verify security code</DialogTitle>
            <DialogDescription>Compare these numbers with {name}&apos;s screen. If they match, nobody has swapped a key in this chat.</DialogDescription>
          </DialogHeader>
          {result === undefined ? <p className="py-6 text-center text-sm text-muted-foreground">Loading…</p>
            : result === "error" ? <p role="alert" className="py-4 text-sm text-destructive">Couldn&apos;t load the security code. Check your connection and try again.</p>
            : !code ? <p className="py-4 text-sm text-muted-foreground">A security code is only shown for private one-to-one chats.</p>
            : (
              <>
                <p className="grid grid-cols-4 gap-x-3 gap-y-2 rounded-xl bg-muted/60 p-4 text-center font-mono text-lg tracking-wider tabular-nums" data-testid="security-code-digits">
                  {code.code.split(" ").map((block, i) => <span key={i}>{block}</span>)}
                </p>
                <p className="flex gap-2 text-xs text-muted-foreground">
                  {code.logChecked ? <ShieldCheck className="size-4 shrink-0 text-emerald-600" /> : <ShieldAlert className="size-4 shrink-0 text-amber-600" />}
                  {code.logChecked
                    ? "The keys were also found in CF Meet's key log. That log is watched by CF Meet itself today, not by an independent group."
                    : "The keys couldn't be checked against CF Meet's key log yet. Comparing the code in person still works."}
                </p>
                <p className="text-xs text-muted-foreground">The code changes when either of you adds or removes a device.</p>
              </>
            )}
        </DialogContent>
      </Dialog>
    </>
  );
}
