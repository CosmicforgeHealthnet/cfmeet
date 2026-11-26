"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { BellRing, Database, EyeOff, ShieldCheck, UserPlus } from "lucide-react";
import { relay } from "@/services/v2/messaging.service";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { communicationsService as hub, type NotificationPreferences } from "@/services/v2/communications.service";
import { communicationsErrorMessage } from "@/lib/communications/errors";
import { cn } from "@/lib/utils";
import { useEffect, useState } from "react";
import { MessagingEngine } from "@/lib/messaging/engine";
import { disableWebPush, enableWebPush, webPushAvailable, webPushEnabled, webPushSupported } from "@/lib/messaging/web-push";

const PREFS_KEY = ["me", "notification-preferences"] as const;
const ROWS: { key: keyof Omit<NotificationPreferences, "version">; label: string; hint: string }[] = [
  { key: "directMessages", label: "Messages", hint: "New messages in one-to-one chats" },
  { key: "groupMessages", label: "Group messages", hint: "New messages in groups" },
  { key: "incomingCalls", label: "Incoming calls", hint: "When someone calls you" },
  { key: "missedCalls", label: "Missed calls", hint: "Calls you didn't answer" },
  { key: "contactRequests", label: "Contact requests", hint: "Someone wants to connect" },
  { key: "groupInvitations", label: "Group invitations", hint: "You're added or invited to a group" },
  { key: "meetingInvitations", label: "Meeting invitations", hint: "You're invited to a meeting" },
];
/** "Anyone with my CF Meet ID can add me" (privacy contactApproval: AUTOMATIC / MANUAL). */
function DirectContact() {
  const client = useQueryClient();
  const privacy = useQuery({ queryKey: ["me", "privacy"], queryFn: () => hub.privacy(), retry: false });
  const save = useMutation({
    mutationFn: (automatic: boolean) => {
      const v = privacy.data!;
      return hub.updatePrivacy(v.version, { ...v.privacy, contactApproval: automatic ? "AUTOMATIC" : "MANUAL" });
    },
    onSuccess: (v) => client.setQueryData(["me", "privacy"], v),
  });
  if (!privacy.data) return privacy.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(privacy.error)}</p> : null;
  const on = (privacy.data.privacy.contactApproval ?? "AUTOMATIC") === "AUTOMATIC";
  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6">
      <div className="flex items-center gap-2"><UserPlus className="size-4 text-muted-foreground" /><h2 className="text-base font-semibold">Contacts</h2></div>
      <button type="button" role="switch" aria-checked={on} disabled={save.isPending} onClick={() => save.mutate(!on)} className="flex w-full items-center gap-3 text-left disabled:opacity-60">
        <span className="min-w-0 flex-1"><span className="block text-sm font-medium">Anyone with my CF Meet ID can add me</span><span className="block text-xs text-muted-foreground">They can chat and call you straight away. Turn this off to approve contact requests first.</span></span>
        <Switch on={on} />
      </button>
      {save.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(save.error)}</p> : null}
    </section>
  );
}

type PresenceSettings = { visibility: "CONVERSATION_MEMBERS" | "HIDDEN"; shareLastSeen: boolean; shareReadReceipts: boolean };
const PRESENCE_KEY = ["messaging", "presence-settings"] as const;

/** WhatsApp's Privacy: online status, last seen and read receipts (Messaging presence settings). */
function PresencePrivacy() {
  const client = useQueryClient();
  const settings = useQuery({ queryKey: PRESENCE_KEY, queryFn: () => relay<PresenceSettings>("GET", "v1/messaging/presence/settings"), retry: false });
  const save = useMutation({
    mutationFn: (next: PresenceSettings) => relay<PresenceSettings>("PUT", "v1/messaging/presence/settings", next),
    onSuccess: (v) => client.setQueryData(PRESENCE_KEY, v),
  });
  const v = settings.data;
  if (!v) return settings.isError ? <p role="alert" className="text-sm text-destructive">{messagingErrorMessage(settings.error)}</p> : null;
  const rows: { key: string; on: boolean; label: string; hint: string; next: PresenceSettings }[] = [
    { key: "online", on: v.visibility === "CONVERSATION_MEMBERS", label: "Show when I'm online", hint: "People you chat with see when you're online.", next: { ...v, visibility: v.visibility === "HIDDEN" ? "CONVERSATION_MEMBERS" : "HIDDEN" } },
    { key: "lastSeen", on: v.shareLastSeen, label: "Share last seen", hint: "People you chat with see when you were last online.", next: { ...v, shareLastSeen: !v.shareLastSeen } },
    { key: "receipts", on: v.shareReadReceipts, label: "Read receipts", hint: "Others see blue ticks when you've read their messages.", next: { ...v, shareReadReceipts: !v.shareReadReceipts } },
  ];
  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6" data-testid="presence-privacy">
      <div className="flex items-center gap-2"><EyeOff className="size-4 text-muted-foreground" /><h2 className="text-base font-semibold">Privacy</h2></div>
      {rows.map((r) => (
        <button key={r.key} type="button" role="switch" aria-checked={r.on} disabled={save.isPending} onClick={() => save.mutate(r.next)} className="flex w-full items-center gap-3 text-left disabled:opacity-60" data-testid={`privacy-${r.key}`}>
          <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{r.label}</span><span className="block text-xs text-muted-foreground">{r.hint}</span></span>
          <Switch on={r.on} />
        </button>
      ))}
      {save.isError ? <p role="alert" className="text-sm text-destructive">{messagingErrorMessage(save.error)}</p> : null}
    </section>
  );
}

const bytes = (n: number | null | undefined) => n == null ? "—" : n < 1024 * 1024 ? `${Math.round(n / 1024)} KB` : n < 1024 ** 3 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${(n / 1024 ** 3).toFixed(2)} GB`;

function Switch({ on }: { on: boolean }) {
  return (
    <span aria-hidden className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", on ? "bg-primary" : "bg-muted-foreground/30")}>
      <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", on ? "left-[1.125rem]" : "left-0.5")} />
    </span>
  );
}

/** "Show notifications on this computer" (Web Push). Hidden when the browser or the server can't do it. */
function BrowserAlerts() {
  const [state, setState] = useState<"loading" | "off" | "on" | "busy" | "hidden">("loading");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!webPushSupported()) { setState("hidden"); return; }
    void (async () => setState(!(await webPushAvailable()) ? "hidden" : (await webPushEnabled()) ? "on" : "off"))();
  }, []);
  if (state === "hidden" || state === "loading") return null;
  const deviceId = MessagingEngine.active?.getSnapshot().deviceId ?? null;
  const toggle = async () => {
    setError(null);
    const was = state;
    setState("busy");
    try {
      if (was === "on") { await disableWebPush(deviceId); setState("off"); }
      else {
        if (!deviceId) throw new Error("Turn on private messaging in Chats first, then come back here.");
        await enableWebPush(deviceId); setState("on");
      }
    } catch (e) { setError(e instanceof Error ? e.message : "That didn't work. Try again."); setState(was); }
  };
  return (
    <div className="border-t border-border pt-3">
      <button type="button" role="switch" aria-checked={state === "on"} disabled={state === "busy"} onClick={() => void toggle()} className="flex w-full items-center gap-3 text-left disabled:opacity-60">
        <span className="min-w-0 flex-1"><span className="block text-sm font-medium">Show notifications on this device</span><span className="block text-xs text-muted-foreground">Get an alert for new messages even when CF Meet isn&apos;t open. Alerts never show the message text.</span></span>
        <Switch on={state === "on"} />
      </button>
      {error ? <p role="alert" className="mt-2 text-sm text-destructive">{error}</p> : null}
    </div>
  );
}

/** Settings → Chats & notifications: what you're notified about, storage used, and how your data is restored. */
export default function ChatSettings() {
  const client = useQueryClient();
  const prefs = useQuery({ queryKey: PREFS_KEY, queryFn: () => hub.notificationPreferences(), retry: false });
  const storage = useQuery({ queryKey: ["me", "storage-summary"], queryFn: () => hub.storageSummary(), retry: false });
  const recovery = useQuery({ queryKey: ["me", "recovery-state"], queryFn: () => hub.recoveryState(), retry: false });
  const save = useMutation({
    mutationFn: (next: NotificationPreferences) => hub.setNotificationPreferences(next),
    onSuccess: (v) => client.setQueryData(PREFS_KEY, v),
  });
  const p = prefs.data;
  const m = storage.data?.messaging;
  const quota = m?.environmentQuotaBytes ?? null;
  const used = (m?.attachmentBytes ?? 0) + (m?.backupBytes ?? 0);

  return (
    <div className="space-y-4">
      <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <div className="flex items-center gap-2"><BellRing className="size-4 text-muted-foreground" /><h2 className="text-base font-semibold">Notifications</h2></div>
        <p className="text-sm text-muted-foreground">Choose what CF Meet tells you about. You can also mute a single chat from its Chat info.</p>
        {prefs.isLoading ? <div className="h-40 animate-pulse rounded-xl bg-muted" /> : null}
        {prefs.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(prefs.error)}</p> : null}
        {p ? (
          <div className="divide-y divide-border">
            {ROWS.map((r) => (
              <button key={r.key} type="button" role="switch" aria-checked={p[r.key]} disabled={save.isPending} onClick={() => save.mutate({ ...p, [r.key]: !p[r.key] })}
                className="flex w-full items-center gap-3 py-3 text-left disabled:opacity-60">
                <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{r.label}</span><span className="block text-xs text-muted-foreground">{r.hint}</span></span>
                <Switch on={p[r.key]} />
              </button>
            ))}
          </div>
        ) : null}
        {save.isError ? <p role="alert" className="text-sm text-destructive">{communicationsErrorMessage(save.error)}</p> : null}
        <BrowserAlerts />
      </section>

      <DirectContact />
      <PresencePrivacy />

      <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6">
        <div className="flex items-center gap-2"><Database className="size-4 text-muted-foreground" /><h2 className="text-base font-semibold">Storage</h2></div>
        {storage.isLoading ? <div className="h-16 animate-pulse rounded-xl bg-muted" /> : null}
        {storage.isError ? <p className="text-sm text-muted-foreground">Storage details aren&apos;t available right now.</p> : null}
        {storage.data ? (
          <>
            {quota ? (
              <div>
                <div className="flex justify-between text-sm"><span>{bytes(used)} used</span><span className="text-muted-foreground">of {bytes(quota)}</span></div>
                <div className="mt-2 h-2 overflow-hidden rounded-full bg-muted"><div className="h-full rounded-full bg-primary" style={{ width: `${Math.min(100, (used / quota) * 100)}%` }} /></div>
              </div>
            ) : null}
            <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-3">
              <div className="rounded-xl bg-muted/50 p-3"><dt className="text-xs text-muted-foreground">Files in chats</dt><dd className="mt-0.5 font-semibold">{m?.attachmentCount ?? 0} · {bytes(m?.attachmentBytes)}</dd></div>
              <div className="rounded-xl bg-muted/50 p-3"><dt className="text-xs text-muted-foreground">Chat backups</dt><dd className="mt-0.5 font-semibold">{bytes(m?.backupBytes)}</dd></div>
              <div className="rounded-xl bg-muted/50 p-3"><dt className="text-xs text-muted-foreground">Starred messages</dt><dd className="mt-0.5 font-semibold">{storage.data.savedMessages.count} of {storage.data.savedMessages.limit}</dd></div>
            </dl>
          </>
        ) : null}
      </section>

      {recovery.data ? (
        <section className="space-y-3 rounded-2xl border border-border bg-card p-5 sm:p-6">
          <div className="flex items-center gap-2"><ShieldCheck className="size-4 text-muted-foreground" /><h2 className="text-base font-semibold">Backup and restore</h2></div>
          <ul className="space-y-2 text-sm">
            <li className="flex justify-between gap-3"><span>Contacts, lists and settings</span><span className="text-right text-muted-foreground">Saved on CF Meet, restored when you sign in</span></li>
            <li className="flex justify-between gap-3"><span>Private messages</span><span className="text-right text-muted-foreground">{recovery.data.messaging?.securityReady ? "Move them from another device of yours" : "Turn on private messaging first"}</span></li>
            <li className="flex justify-between gap-3"><span>Devices</span><span className="text-right text-muted-foreground">{recovery.data.devices?.active ?? 0} active</span></li>
          </ul>
        </section>
      ) : null}
    </div>
  );
}
