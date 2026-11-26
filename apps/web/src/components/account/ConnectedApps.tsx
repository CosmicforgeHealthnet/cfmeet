"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plug } from "lucide-react";
import apiV2 from "@/services/v2/api";
import { getErrorMessage } from "@/utils/error";

type ConnectedApp = {
  grantId: string;
  appId: string;
  name: string | null;
  developerName: string | null;
  logoUrl: string | null;
  target: "USER" | "ORGANIZATION" | string;
  organizationId: string | null;
  scopes: string[];
  status: string;
  createdAt: string | null;
  hasMeetingSurface: boolean;
};

const KEY = ["me", "apps"] as const;

/** A scope as a person would read it ("meetings.read" -> "Read your meetings"). */
function scopeLabel(scope: string): string {
  const [area, action] = scope.split(".");
  const what = (area ?? scope).replaceAll("_", " ").replaceAll("-", " ");
  if (action === "read") return `See your ${what}`;
  if (action === "write" || action === "send" || action === "manage") return `Manage your ${what}`;
  return scope;
}

/**
 * Settings → Connected apps: apps you allowed to use your CF Meet account (GET /me/apps),
 * with what they may do, and Disconnect (DELETE /me/apps/:grantId).
 */
export default function ConnectedApps() {
  const client = useQueryClient();
  const apps = useQuery({ queryKey: KEY, queryFn: async () => (await apiV2.get<{ items: ConnectedApp[] }>("/me/apps")).data.items ?? [], retry: false });
  const [confirm, setConfirm] = useState<string | null>(null);
  const revoke = useMutation({
    mutationFn: async (grantId: string) => { await apiV2.delete(`/me/apps/${encodeURIComponent(grantId)}`); },
    onSettled: () => client.invalidateQueries({ queryKey: KEY }),
  });
  const status = (apps.error as { response?: { status?: number } } | null)?.response?.status;

  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6" data-testid="connected-apps">
      <p className="text-sm text-muted-foreground">Apps you allowed to use your CF Meet account. Disconnect one and it loses access right away.</p>
      {apps.isPending ? <div className="h-16 animate-pulse rounded-xl bg-muted" /> : null}
      {apps.isError ? (
        <p role={status === 404 ? undefined : "alert"} className={status === 404 ? "text-sm text-muted-foreground" : "text-sm text-destructive"}>
          {status === 404 ? "Connecting apps isn't available yet." : getErrorMessage(apps.error, "Your connected apps couldn't be loaded.")}
        </p>
      ) : null}
      {apps.data && apps.data.length === 0 ? (
        <div className="flex flex-col items-center py-8 text-center">
          <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Plug className="size-5" /></span>
          <p className="mt-3 text-sm font-medium">No connected apps</p>
          <p className="text-sm text-muted-foreground">Apps you sign in to with CF Meet will appear here.</p>
        </div>
      ) : null}
      <ul className="space-y-2">
        {(apps.data ?? []).map((app) => (
          <li key={app.grantId} className="rounded-xl p-3 ring-1 ring-border" data-testid="connected-app">
            <div className="flex items-center gap-3">
              {app.logoUrl
                // eslint-disable-next-line @next/next/no-img-element
                ? <img src={app.logoUrl} alt="" className="size-10 rounded-xl object-cover" />
                : <span className="flex size-10 items-center justify-center rounded-xl bg-primary/10 text-primary"><Plug className="size-5" /></span>}
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">{app.name ?? "Unnamed app"}</span>
                <span className="block truncate text-xs text-muted-foreground">
                  {app.developerName ? `By ${app.developerName}` : "Developer not listed"}
                  {app.target === "ORGANIZATION" ? " · for an organization" : ""}
                  {app.createdAt ? ` · since ${new Date(app.createdAt).toLocaleDateString()}` : ""}
                </span>
              </span>
              {confirm === app.grantId ? (
                <span className="flex gap-1.5">
                  <button type="button" onClick={() => setConfirm(null)} className="h-9 rounded-full px-3 text-sm font-semibold ring-1 ring-border hover:bg-muted">Cancel</button>
                  <button type="button" disabled={revoke.isPending} onClick={() => revoke.mutate(app.grantId, { onSettled: () => setConfirm(null) })} data-testid="disconnect-app-confirm"
                    className="h-9 rounded-full bg-destructive px-3 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-60">Disconnect</button>
                </span>
              ) : (
                <button type="button" onClick={() => setConfirm(app.grantId)} data-testid="disconnect-app"
                  className="h-9 rounded-full px-3 text-sm font-semibold text-destructive ring-1 ring-border hover:bg-destructive/10">Disconnect</button>
              )}
            </div>
            {app.scopes.length ? (
              <ul className="mt-2 flex flex-wrap gap-1.5 pl-[3.25rem]">
                {app.scopes.map((s) => <li key={s} className="rounded-full bg-muted px-2.5 py-0.5 text-xs text-muted-foreground" title={s}>{scopeLabel(s)}</li>)}
              </ul>
            ) : null}
          </li>
        ))}
      </ul>
      {revoke.isError ? <p role="alert" className="text-sm text-destructive">{getErrorMessage(revoke.error, "The app couldn't be disconnected. Try again.")}</p> : null}
    </section>
  );
}
