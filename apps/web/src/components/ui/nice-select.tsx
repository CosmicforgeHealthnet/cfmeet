"use client";

import { useEffect, useId, useRef, useState } from "react";
import { Check, ChevronDown } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { cn } from "@/lib/utils";

export type NiceOption<T extends string> = { value: T; label: string; hint?: string; icon?: LucideIcon; disabled?: boolean };

/**
 * The app's dropdown: a designed trigger and menu (never the browser's native list),
 * with optional icons and one-line hints, a check on the selected option and full
 * keyboard support (↑ ↓ Enter Esc).
 */
export function NiceSelect<T extends string>({
  label,
  value,
  options,
  onChange,
  placeholder = "Select…",
  disabled,
  className,
}: {
  label?: string;
  value: T | "";
  options: NiceOption<T>[];
  onChange: (value: T) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const current = options.find((o) => o.value === value);
  const CurrentIcon = current?.icon;

  useEffect(() => {
    if (!open) return;
    setActive(Math.max(0, options.findIndex((o) => o.value === value)));
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open, options, value]);

  const pick = (o: NiceOption<T> | undefined) => {
    if (!o || o.disabled) return;
    onChange(o.value);
    setOpen(false);
  };

  return (
    <div className={cn("grid gap-2", className)} ref={ref}>
      {label && <span id={`${id}-label`} className="text-sm font-medium text-foreground">{label}</span>}
      <div className="relative">
        <button
          type="button"
          disabled={disabled}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-labelledby={label ? `${id}-label` : undefined}
          onClick={() => setOpen((o) => !o)}
          onKeyDown={(e) => {
            if (!open && (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ")) { e.preventDefault(); setOpen(true); return; }
            if (!open) return;
            if (e.key === "Escape") { e.preventDefault(); setOpen(false); }
            else if (e.key === "ArrowDown") { e.preventDefault(); setActive((a) => Math.min(a + 1, options.length - 1)); }
            else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
            else if (e.key === "Enter") { e.preventDefault(); pick(options[active]); }
          }}
          className="flex h-11 w-full items-center gap-2.5 rounded-xl border border-border bg-background px-3 text-left text-sm transition-colors hover:bg-muted/50 focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary disabled:opacity-50"
        >
          {CurrentIcon && <CurrentIcon className="size-4 shrink-0 text-primary" aria-hidden />}
          <span className={cn("min-w-0 flex-1 truncate", current ? "font-medium text-foreground" : "text-muted-foreground")}>{current?.label ?? placeholder}</span>
          <ChevronDown className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} aria-hidden />
        </button>
        {open && (
          <ul role="listbox" aria-labelledby={label ? `${id}-label` : undefined} className="absolute left-0 right-0 top-full z-50 mt-2 max-h-72 overflow-y-auto rounded-xl border border-border bg-popover p-1.5 shadow-xl">
            {options.map((o, i) => {
              const Icon = o.icon;
              const selected = o.value === value;
              return (
                <li key={o.value} role="option" aria-selected={selected} aria-disabled={o.disabled}>
                  <button
                    type="button"
                    disabled={o.disabled}
                    onMouseEnter={() => setActive(i)}
                    onClick={() => pick(o)}
                    className={cn("flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left disabled:opacity-50", i === active && "bg-muted")}
                  >
                    {Icon && (
                      <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg", selected ? "bg-primary text-primary-foreground" : "bg-primary/10 text-primary")}>
                        <Icon className="size-4" aria-hidden />
                      </span>
                    )}
                    <span className="min-w-0 flex-1">
                      <span className="block text-sm font-medium text-foreground">{o.label}</span>
                      {o.hint && <span className="block truncate text-xs text-muted-foreground">{o.hint}</span>}
                    </span>
                    {selected && <Check className="size-4 shrink-0 text-primary" aria-hidden />}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
