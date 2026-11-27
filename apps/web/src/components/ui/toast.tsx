"use client";

/**
 * Small app-wide toasts: `toast("Profile photo updated")` from anywhere; <Toaster /> is
 * mounted once in the dashboard layout. Bottom centre, above the phone tab bar.
 */
import { useSyncExternalStore } from "react";
import { CircleAlert, CircleCheck } from "lucide-react";
import { cn } from "@/lib/utils";

type Toast = { id: number; message: string; tone: "success" | "error" };
let toasts: Toast[] = [];
let nextId = 1;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

export function toast(message: string, tone: Toast["tone"] = "success") {
  const id = nextId++;
  toasts = [...toasts.slice(-2), { id, message, tone }];
  emit();
  setTimeout(() => { toasts = toasts.filter((t) => t.id !== id); emit(); }, 3500);
}

const subscribe = (l: () => void) => { listeners.add(l); return () => { listeners.delete(l); }; };
const EMPTY: Toast[] = [];

export function Toaster() {
  const list = useSyncExternalStore(subscribe, () => toasts, () => EMPTY);
  return (
    <div aria-live="polite" className="pointer-events-none fixed inset-x-0 bottom-24 z-[100] flex flex-col items-center gap-2 px-4 md:bottom-6">
      {list.map((t) => (
        <div key={t.id} role={t.tone === "error" ? "alert" : "status"}
          className="pointer-events-auto flex max-w-sm items-center gap-2.5 rounded-full bg-foreground px-4 py-2.5 text-sm font-medium text-background shadow-xl animate-in fade-in slide-in-from-bottom-2">
          {t.tone === "error" ? <CircleAlert className="size-4 shrink-0 text-red-400" /> : <CircleCheck className={cn("size-4 shrink-0 text-emerald-400")} />}
          {t.message}
        </div>
      ))}
    </div>
  );
}
