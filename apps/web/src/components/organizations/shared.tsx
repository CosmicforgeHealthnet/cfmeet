"use client";

import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { organizationErrorText } from "@/lib/organizations/organization-core";

/** A request failure in the organization area, rendered from the stable error envelope. */
export function OrgError({ error, retry }: { error: unknown; retry?: () => void }) {
  return (
    <div role="alert" className="rounded-lg border border-destructive/30 p-3 text-sm text-destructive">
      {organizationErrorText(error)}
      {retry && (
        <Button className="ml-3" variant="outline" size="sm" onClick={retry}>
          Retry
        </Button>
      )}
    </div>
  );
}

export function Section({ title, description, children, actions }: { title: string; description?: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <section className="space-y-4 rounded-xl border border-border bg-card p-4">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h2 className="font-semibold">{title}</h2>
          {description && <p className="text-sm text-muted-foreground">{description}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

export function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-0.5">
      <dt className="text-xs font-medium uppercase tracking-wide text-muted-foreground">{label}</dt>
      <dd className="break-words text-sm">{children ?? "—"}</dd>
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
}) {
  const id = `f-${label.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
  const className = "w-full rounded-md border border-input bg-background px-3 py-2 text-sm";
  return (
    <div className="grid gap-1 text-sm">
      <label htmlFor={id} className="font-medium">
        {label}
      </label>
      {multiline ? (
        <textarea id={id} className={`${className} min-h-24`} value={value} maxLength={maxLength} onChange={(e) => onChange(e.target.value)} aria-invalid={!!error} />
      ) : (
        <input id={id} type={type} className={`${className} h-10`} value={value} required={required} maxLength={maxLength} onChange={(e) => onChange(e.target.value)} aria-invalid={!!error} />
      )}
      {hint && !error && <span className="text-xs text-muted-foreground">{hint}</span>}
      {error && <span className="text-xs text-destructive">{error}</span>}
    </div>
  );
}

export function formatDateTime(value: string | null | undefined): string {
  if (!value) return "—";
  const date = new Date(value);
  return Number.isFinite(+date) ? date.toLocaleString() : value;
}
