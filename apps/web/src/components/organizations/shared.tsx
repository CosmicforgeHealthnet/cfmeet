"use client";

import type { ReactNode } from "react";
import { AlertCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import { organizationErrorText } from "@/lib/organizations/organization-core";

/** A request failure in the organization area, rendered from the stable error envelope. */
export function OrgError({ error, retry }: { error: unknown; retry?: () => void }) {
  return (
    <div role="alert" className="flex items-start gap-3 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive">
      <AlertCircle className="mt-0.5 size-4 shrink-0" />
      <span className="min-w-0 flex-1">{organizationErrorText(error)}</span>
      {retry && <button type="button" onClick={retry} className="shrink-0 font-semibold hover:underline">Try again</button>}
    </div>
  );
}

export function Section({ title, description, children, actions }: { title: string; description?: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <section className="space-y-5 rounded-2xl border border-border bg-card p-5 sm:p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-base font-semibold text-foreground">{title}</h2>
          {description && <p className="mt-0.5 text-sm text-muted-foreground">{description}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

export function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-1">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="break-words text-sm font-medium text-foreground">{children ?? "—"}</dd>
    </div>
  );
}

export function TextField({
  label,
  value,
  onChange,
  error,
  type = "text",
  hint,
  required,
  maxLength,
  multiline,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  type?: string;
  hint?: string;
  required?: boolean;
  maxLength?: number;
  multiline?: boolean;
  placeholder?: string;
}) {
  const id = `f-${label.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
  const className = cn(
    "w-full rounded-xl border bg-background px-3.5 text-sm text-foreground outline-none transition-colors placeholder:text-muted-foreground focus:border-primary focus:ring-1 focus:ring-primary",
    error ? "border-destructive" : "border-border",
  );
  return (
    <div className="grid gap-1.5 text-sm">
      <label htmlFor={id} className="font-medium text-foreground">
        {label}
      </label>
      {multiline ? (
        <textarea id={id} className={`${className} min-h-24 py-2.5`} value={value} maxLength={maxLength} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} aria-invalid={!!error} />
      ) : (
        <input id={id} type={type} className={`${className} h-11`} value={value} required={required} maxLength={maxLength} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} aria-invalid={!!error} />
      )}
      {hint && !error && <span className="text-xs text-muted-foreground">{hint}</span>}
      {error && <span className="text-xs text-destructive">{error}</span>}
    </div>
  );
}

export function formatDateTime(value: string | null | undefined): string {
  if (!value) return "—";
  const date = new Date(value);
  return Number.isFinite(+date) ? date.toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : value;
}

/** Organization mark: rounded square with initials (matches the workspace switcher). */
export function OrgMark({ name, size = "size-12", className }: { name: string; size?: string; className?: string }) {
  const letters = name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
  return <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-xl bg-violet-600 font-semibold text-white", size, className)}>{letters}</span>;
}
