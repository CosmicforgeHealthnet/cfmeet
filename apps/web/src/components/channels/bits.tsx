"use client";

import { BadgeCheck, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { useMediaUrl } from "@/hooks/v2/useSocial";
import { mediaView, type WireMedia } from "@/services/v2/channels.service";
import { colorFor, initials } from "@/components/updates/shared";

/** The verification badge (granted only by CF Meet). */
export function VerifiedBadge({ className, label = "Verified channel" }: { className?: string; label?: string }) {
  return <BadgeCheck aria-label={label} role="img" className={cn("inline size-4 shrink-0 fill-primary text-primary-foreground", className)} data-testid="verified-badge" />;
}

/** A channel's picture, or its initials. */
export function ChannelAvatar({ name, avatar, size = "size-10" }: { name: string; avatar: WireMedia | null | undefined; size?: string }) {
  const { url } = useMediaUrl(avatar ? mediaView(avatar) : null);
  if (url) {
    // eslint-disable-next-line @next/next/no-img-element
    return <img src={url} alt="" className={cn("shrink-0 rounded-full object-cover", size)} />;
  }
  return (
    <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white", size)} style={{ backgroundColor: colorFor(name) }}>
      {initials(name)}
    </span>
  );
}

/** A plain modal sheet (bottom on phones, centred on wider screens). */
export function Sheet({ title, onClose, children, wide }: { title: string; onClose: () => void; children: React.ReactNode; wide?: boolean }) {
  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center bg-black/50 sm:items-center" role="dialog" aria-modal="true" aria-label={title} onClick={onClose}>
      <div className={cn("max-h-[90dvh] w-full overflow-y-auto rounded-t-3xl bg-background p-4 shadow-2xl sm:rounded-3xl", wide ? "sm:max-w-2xl" : "sm:max-w-md")} onClick={(e) => e.stopPropagation()}>
        <div className="mb-3 flex items-center gap-2">
          <h2 className="min-w-0 flex-1 truncate text-lg font-semibold">{title}</h2>
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
        </div>
        {children}
      </div>
    </div>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: string | null; children: React.ReactNode }) {
  return (
    <label className="block space-y-1">
      <span className="text-sm font-medium">{label}</span>
      {children}
      {hint ? <span className="block text-xs text-muted-foreground">{hint}</span> : null}
    </label>
  );
}

export const inputClass = "h-11 w-full rounded-xl border border-border bg-background px-3 text-[15px] focus:outline-none focus:ring-2 focus:ring-primary/40";

export function Toggle({ checked, onChange, label, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <label className={cn("flex items-center justify-between gap-3 py-2", disabled && "opacity-50")}>
      <span className="text-sm">{label}</span>
      <input type="checkbox" className="size-5 accent-primary" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
    </label>
  );
}
