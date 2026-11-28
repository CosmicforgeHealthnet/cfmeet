"use client";

import { cn } from "@/lib/utils";
import type { PersonView } from "@/services/v2/social.service";

const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
export const colorFor = (raw: string) => COLORS[([...raw.trim().toLowerCase()].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
export const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

/** Text status backgrounds (WhatsApp-like) and fonts. */
export const BACKGROUNDS = ["#128C7E", "#2563eb", "#7c3aed", "#db2777", "#ea580c", "#0f766e", "#334155", "#b45309", "#be123c", "#4338ca"];
export const FONTS = ["font-sans", "font-serif", "font-mono", "font-sans italic", "font-sans font-black uppercase tracking-wide"];

export function PersonAvatar({ person, name, size = "size-12", className, ring }: { person?: PersonView | null; name?: string; size?: string; className?: string; ring?: "unseen" | "seen" | null }) {
  const label = person?.name ?? name ?? "?";
  const inner = person?.avatarUrl ? (
    // eslint-disable-next-line @next/next/no-img-element
    <img src={person.avatarUrl} alt="" className={cn("rounded-full object-cover", size)} />
  ) : (
    <span aria-hidden className={cn("flex items-center justify-center rounded-full text-sm font-semibold text-white", size)} style={{ backgroundColor: colorFor(label) }}>{initials(label)}</span>
  );
  if (!ring) return <span className={cn("inline-flex shrink-0", className)}>{inner}</span>;
  return (
    <span className={cn("inline-flex shrink-0 rounded-full p-[3px]", ring === "unseen" ? "bg-emerald-500" : "bg-muted-foreground/30", className)}>
      <span className="flex rounded-full bg-background p-[2px]">{inner}</span>
    </span>
  );
}

export function whenText(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === now.toDateString()) return `Today, ${time}`;
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `Yesterday, ${time}`;
  return d.toLocaleDateString([], { day: "numeric", month: "short" }) + `, ${time}`;
}

export function shortWhen(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { day: "2-digit", month: "2-digit", year: "numeric" });
}

export function errorText(e: unknown, fallback = "Something went wrong. Try again."): string {
  const data = (e as { response?: { data?: { error?: { message?: string }; message?: string } } })?.response?.data;
  return data?.error?.message ?? data?.message ?? fallback;
}
