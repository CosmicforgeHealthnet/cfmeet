"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { History, Laptop, ShieldCheck, ShieldQuestion, Smartphone, X } from "lucide-react";
import apiV2 from "@/services/v2/api";
import { accountDevice } from "@/lib/auth/account-device";
import { apiErrorOf } from "@/lib/account/account-core";
import { cn } from "@/lib/utils";
import type { DevicePolicyView, DeviceView, PendingLinkView, SecurityEventRecord } from "@/types/generated/meeting-service";

const KEYS = { policy: ["me", "device-policy"], devices: ["me", "devices"], links: ["me", "device-links"], events: ["me", "security-events"] } as const;

const ago = (iso: string) => {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(iso).toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" });
};

/** Plain wording for the account security log. Unknown types fall back to a readable form. */
const EVENT_TEXT: Record<string, string> = {
  SIGN_IN: "Signed in", SIGN_IN_FAILED: "Sign-in failed", SIGN_OUT: "Signed out", PASSWORD_CHANGED: "Password changed",
  DEVICE_DECLARED: "New browser or phone signed in", DEVICE_TRUSTED: "Device approved", DEVICE_REVOKED: "Device removed",
  LINK_REQUESTED: "A new device asked to be linked", LINK_APPROVED: "A new device was approved", LINK_REJECTED: "A link request was turned down",
  LINK_COMPLETED: "A new device was linked", LINK_CANCELLED: "A link request was cancelled", EMAIL_STEP_UP_SENT: "Verification code emailed",
  SESSION_REVOKED: "Signed out remotely", ALL_SESSIONS_REVOKED: "Signed out everywhere",
};
const eventText = (t: string) => EVENT_TEXT[t] ?? t.toLowerCase().replaceAll("_", " ").replace(/^./, (c) => c.toUpperCase());

const deviceName = (d: DeviceView) => d.label || (d.deviceClass === "MOBILE" ? "Phone" : "Browser");

/**
 * Settings → Security: the devices linked to this account (WhatsApp "Linked devices"), link
 * requests waiting for an answer, and the account's recent security activity.
 */
export default function LinkedDevices() {
  const client = useQueryClient();
  const policy = useQuery({ queryKey: KEYS.policy, queryFn: async () => (await apiV2.get<DevicePolicyView>("/me/device-policy")).data, retry: false });
  const devices = useQuery({ queryKey: KEYS.devices, queryFn: async () => (await apiV2.get<{ devices: DeviceView[] }>("/me/devices")).data.devices, retry: false });
  const links = useQuery({ queryKey: KEYS.links, queryFn: async () => (await apiV2.get<{ links: PendingLinkView[] }>("/me/device-links")).data.links, retry: false, refetchInterval: 10_000 });
  const events = useQuery({ queryKey: KEYS.events, queryFn: async () => (await apiV2.get<{ events: SecurityEventRecord[] }>("/me/security-events")).data.events, retry: false });
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);
  const refresh = () => { for (const k of Object.values(KEYS)) void client.invalidateQueries({ queryKey: k }); };
  const fail = (e: unknown) => setError(apiErrorOf(e).message ?? "That didn't work. Try again.");
  const remove = useMutation({ mutationFn: (id: string) => accountDevice.revokeDevice(id), onSuccess: () => { setConfirm(null); refresh(); }, onError: fail });
  const reject = useMutation({ mutationFn: (id: string) => accountDevice.rejectLink(id), onSuccess: refresh, onError: fail });

  const active = (devices.data ?? []).filter((d) => d.state === "ACTIVE").sort((a, b) => Number(b.current) - Number(a.current));
  const me = active.find((d) => d.current);
  const canManage = me?.trust === "TRUSTED";
  const p = policy.data;

  return (
    <div className="space-y-4">
      <section className="space-y-4 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <div>
          <h2 className="text-base font-semibold">Linked devices</h2>
          <p className="mt-0.5 text-sm text-muted-foreground">
            Browsers and phones that can use your private messages.
            {p ? ` ${p.activeWebDevices} of ${p.maxWebDevices} browsers in use.` : ""}
          </p>
        </div>
        {error && <p role="alert" className="flex items-start gap-2 rounded-xl bg-destructive/10 px-4 py-3 text-sm text-destructive"><span className="flex-1">{error}</span><button type="button" aria-label="Dismiss" onClick={() => setError(null)}><X className="size-4" /></button></p>}
        {devices.isLoading && <div className="space-y-2">{[0, 1].map((i) => <div key={i} className="h-16 animate-pulse rounded-xl bg-muted" />)}</div>}
        {devices.isError && <p role="alert" className="text-sm text-destructive">Your devices couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void devices.refetch()}>Try again</button></p>}
        {active.length > 0 && (
          <ul className="divide-y divide-border">
            {active.map((d) => {
              const Icon = d.deviceClass === "MOBILE" ? Smartphone : Laptop;
              const trusted = d.trust === "TRUSTED";
              return (
                <li key={d.deviceId} className="flex items-center gap-3 py-3">
                  <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-xl", d.current ? "bg-primary/10 text-primary" : "bg-muted text-muted-foreground")}><Icon className="size-5" /></span>
                  <div className="min-w-0 flex-1">
                    <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm font-semibold">
                      <span className="truncate">{deviceName(d)}</span>
                      {d.current && <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] font-semibold text-emerald-700 dark:text-emerald-400">This device</span>}
                      {d.primary && <span className="rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-semibold text-primary">Main phone</span>}
                    </p>
                    <p className="flex items-start gap-1 text-xs text-muted-foreground">
                      {trusted ? <ShieldCheck className="mt-px size-3.5 shrink-0 text-emerald-600" /> : <ShieldQuestion className="mt-px size-3.5 shrink-0 text-amber-600" />}
                      <span className="min-w-0">{trusted ? "Approved" : "Not approved for private messages"} · {d.current ? "active now" : `last active ${ago(d.lastSeenAt)}`}</span>
                    </p>
                  </div>
                  {!d.current && canManage ? (
                    confirm === d.deviceId ? (
                      <div className="flex shrink-0 gap-1">
                        <button type="button" onClick={() => setConfirm(null)} className="h-9 rounded-xl px-3 text-sm font-semibold hover:bg-muted">Keep</button>
                        <button type="button" disabled={remove.isPending} onClick={() => remove.mutate(d.deviceId)} className="h-9 rounded-xl bg-destructive px-3 text-sm font-semibold text-white hover:bg-destructive/90 disabled:opacity-50">Remove</button>
                      </div>
                    ) : (
                      <button type="button" onClick={() => setConfirm(d.deviceId)} className="h-9 shrink-0 rounded-xl px-3.5 text-sm font-semibold text-muted-foreground hover:bg-muted hover:text-foreground">Remove</button>
                    )
                  ) : null}
                </li>
              );
            })}
          </ul>
        )}
        {me && !canManage ? <p className="rounded-xl bg-muted/60 px-4 py-3 text-sm text-muted-foreground">Approve this browser from a device you already use (open Chats there) to manage your other devices from here.</p> : null}
      </section>

      {(links.data?.length ?? 0) > 0 && (
        <section className="space-y-3 rounded-2xl border border-amber-500/30 bg-amber-500/5 p-5 sm:p-6">
          <div>
            <h2 className="text-base font-semibold">Waiting for your approval</h2>
            <p className="mt-0.5 text-sm text-muted-foreground">To approve one, open Chats on this device and type the code the new device shows. Turn down anything you don&apos;t recognise.</p>
          </div>
          <ul className="divide-y divide-border">
            {links.data!.map((l) => (
              <li key={l.linkId} className="flex items-center gap-3 py-3">
                <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground">{l.deviceClass === "MOBILE" ? <Smartphone className="size-5" /> : <Laptop className="size-5" />}</span>
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-semibold">{l.label || (l.deviceClass === "MOBILE" ? "A phone" : "A browser")}</p>
                  <p className="text-xs text-muted-foreground">Asked {ago(l.createdAt)} · expires {ago(l.expiresAt).replace(" ago", "")}</p>
                </div>
                <button type="button" disabled={reject.isPending} onClick={() => reject.mutate(l.linkId)} className="h-9 shrink-0 rounded-xl px-3.5 text-sm font-semibold text-destructive hover:bg-destructive/10 disabled:opacity-50">Turn down</button>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <div className="flex items-center gap-2">
          <History className="size-4 text-muted-foreground" />
          <h2 className="text-base font-semibold">Security activity</h2>
        </div>
        {events.isLoading && <div className="h-24 animate-pulse rounded-xl bg-muted" />}
        {events.isError && <p className="text-sm text-muted-foreground">Activity couldn&apos;t load.</p>}
        {events.data && events.data.length === 0 && <p className="text-sm text-muted-foreground">Nothing yet.</p>}
        {events.data && events.data.length > 0 && (
          <ul className="space-y-2">
            {events.data.slice(0, 15).map((e) => (
              <li key={e.eventId} className="flex items-start gap-3 text-sm">
                <span className={cn("mt-1.5 size-2 shrink-0 rounded-full", e.outcome === "SUCCESS" || e.outcome === "ALLOWED" ? "bg-emerald-500" : "bg-amber-500")} />
                <span className="min-w-0 flex-1">{eventText(e.eventType)}</span>
                <span className="shrink-0 text-xs text-muted-foreground">{ago(e.occurredAt)}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
