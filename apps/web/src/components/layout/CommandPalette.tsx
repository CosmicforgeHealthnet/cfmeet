"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { CalendarPlus, CornerDownLeft, LogIn, Search } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useDashboardNavigation } from "@/lib/use-dashboard-navigation";
import { cn } from "@/lib/utils";

type Command = { id: string; label: string; hint: string; icon: LucideIcon; href: string };

/** Opens the command bar from anywhere (top bar button, keyboard shortcut). */
export const OPEN_COMMANDS_EVENT = "cfmeet:open-commands";

/**
 * Ctrl/⌘ + K command bar: jump to any section or run a common action. Only real routes
 * the signed-in user can open (the navigation is already capability-filtered).
 */
export default function CommandPalette() {
  const router = useRouter();
  const menu = useDashboardNavigation();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const commands = useMemo<Command[]>(
    () => [
      { id: "schedule", label: "Schedule a meeting", hint: "Action", icon: CalendarPlus, href: "/dashboard/meetings/new" },
      { id: "join", label: "Join a meeting with a code", hint: "Action", icon: LogIn, href: "/join" },
      ...menu.map((m) => ({ id: m.href, label: `Go to ${m.label}`, hint: "Page", icon: m.icon, href: m.href })),
    ],
    [menu],
  );

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q ? commands.filter((c) => c.label.toLowerCase().includes(q)) : commands;
  }, [commands, query]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((o) => !o);
      }
    };
    const onOpen = () => setOpen(true);
    window.addEventListener("keydown", onKey);
    window.addEventListener(OPEN_COMMANDS_EVENT, onOpen);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener(OPEN_COMMANDS_EVENT, onOpen);
    };
  }, []);

  useEffect(() => {
    if (open) {
      setQuery("");
      setIndex(0);
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  if (!open) return null;

  const run = (c: Command | undefined) => {
    if (!c) return;
    setOpen(false);
    router.push(c.href);
  };

  return (
    <div className="fixed inset-0 z-[60] flex items-start justify-center bg-black/40 px-4 pt-[12vh] backdrop-blur-[2px]" onClick={() => setOpen(false)}>
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Search and commands"
        className="w-full max-w-xl overflow-hidden rounded-2xl border border-border bg-card shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 border-b border-border px-4">
          <Search className="size-5 text-muted-foreground" aria-hidden />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setIndex(0);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") setOpen(false);
              else if (e.key === "ArrowDown") {
                e.preventDefault();
                setIndex((i) => Math.min(i + 1, results.length - 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setIndex((i) => Math.max(i - 1, 0));
              } else if (e.key === "Enter") run(results[index]);
            }}
            placeholder="Search pages and actions…"
            aria-label="Search pages and actions"
            className="h-14 flex-1 bg-transparent text-base text-foreground outline-none placeholder:text-muted-foreground"
          />
          <kbd className="rounded-md border border-border px-1.5 py-0.5 text-[11px] text-muted-foreground">Esc</kbd>
        </div>
        <ul role="listbox" className="max-h-80 overflow-y-auto p-2">
          {results.length === 0 && <li className="px-3 py-6 text-center text-sm text-muted-foreground">No matches</li>}
          {results.map((c, i) => {
            const Icon = c.icon;
            return (
              <li key={c.id} role="option" aria-selected={i === index}>
                <button
                  type="button"
                  onMouseEnter={() => setIndex(i)}
                  onClick={() => run(c)}
                  className={cn(
                    "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm",
                    i === index ? "bg-primary/10 text-foreground" : "text-foreground",
                  )}
                >
                  <Icon className={cn("size-4", i === index ? "text-primary" : "text-muted-foreground")} aria-hidden />
                  <span className="flex-1">{c.label}</span>
                  <span className="text-xs text-muted-foreground">{c.hint}</span>
                  {i === index && <CornerDownLeft className="size-3.5 text-muted-foreground" aria-hidden />}
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}
