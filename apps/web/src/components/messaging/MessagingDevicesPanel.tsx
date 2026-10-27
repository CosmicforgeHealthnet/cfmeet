"use client";

import { useState, useSyncExternalStore } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useEndMessagingSessions, useMessagingDevices, useRevokeMessagingDevice } from "@/hooks/v2/useMessaging";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import type { MessagingEngine } from "@/lib/messaging/engine";
import type { DeviceRevocationReason, MessagingDevice } from "@/types/messaging";

export default function MessagingDevicesPanel({ engine }: { engine: MessagingEngine }) {
  const snapshot = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [expanded, setExpanded] = useState(false);
  const [selected, setSelected] = useState<MessagingDevice | "sessions" | null>(null);
  const [reason, setReason] = useState<DeviceRevocationReason>("USER_REQUESTED");
  const [notice, setNotice] = useState<string | null>(null);
  const devices = useMessagingDevices(expanded ? snapshot.identity?.principalId ?? "" : "");
  const revoke = useRevokeMessagingDevice(snapshot.identity?.principalId ?? "");
  const sessions = useEndMessagingSessions(engine);
  if (!snapshot.identity) return null;
  const pending = revoke.isPending || sessions.isPending;
  const error = revoke.error || sessions.error;
  return <section className="space-y-3 rounded-2xl border border-border bg-card p-4" aria-label="Messaging devices and sessions">
    <div className="flex flex-wrap items-center gap-2">
      <Button variant="outline" size="sm" onClick={() => setExpanded(!expanded)} aria-expanded={expanded}>Manage messaging devices</Button>
      <Button variant="outline" size="sm" onClick={() => { sessions.reset(); revoke.reset(); setSelected("sessions"); }}>End sessions on all devices</Button>
      {snapshot.sessionsPaused ? <Button size="sm" onClick={() => { engine.resumeSession(); setNotice(null); }}>Reconnect this browser</Button> : null}
    </div>
    {notice ? <p role="status" className="text-sm text-muted-foreground">{notice}</p> : null}
    {snapshot.sessionsPaused ? <p className="text-sm text-muted-foreground">Live updates are paused here. Devices remain registered; signed-in devices can establish new sessions.</p> : null}
    {expanded ? <div>
      {devices.isPending ? <p className="text-sm text-muted-foreground">Loading devices…</p> : devices.error ? <p role="alert" className="text-sm text-destructive">{messagingErrorMessage(devices.error)} <button className="underline" onClick={() => void devices.refetch()}>Retry</button></p> : <ul className="divide-y divide-border">
        {devices.data?.devices.map((device) => <li key={device.deviceId} className="flex flex-wrap items-center justify-between gap-2 py-2 text-sm">
          <div><p className="font-medium">{device.displayLabel || "Messaging device"}{device.deviceId === snapshot.deviceId ? " (this browser)" : ""}</p><p className="text-xs text-muted-foreground">{device.status === "ACTIVE" ? "Active" : "Revoked"}</p></div>
          {device.status === "ACTIVE" && device.deviceId !== snapshot.deviceId ? <Button variant="outline" size="sm" disabled={pending} onClick={() => { revoke.reset(); sessions.reset(); setReason("USER_REQUESTED"); setSelected(device); }}>Revoke device</Button> : null}
        </li>)}
        {devices.data?.devices.length === 0 ? <li className="py-2 text-sm text-muted-foreground">No registered messaging devices.</li> : null}
      </ul>}
    </div> : null}
    <Dialog open={selected !== null} onOpenChange={(open) => { if (!open && !pending) setSelected(null); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{selected === "sessions" ? "End every messaging session?" : "Revoke this messaging device?"}</DialogTitle>
          <DialogDescription>{selected === "sessions" ? "This ends existing realtime sessions on all devices, including this browser. It keeps devices, encryption keys, and message history. Signed-in devices can establish new sessions." : "This device will no longer receive encrypted messages or use existing sessions. Its keys will be removed by that client when it learns of the revocation."}</DialogDescription>
        </DialogHeader>
        {selected && selected !== "sessions" ? <label className="space-y-2 text-sm">Reason<select className="mt-2 h-10 w-full rounded-md border border-input bg-background px-3" value={reason} onChange={(event) => setReason(event.target.value as DeviceRevocationReason)}>
          <option value="USER_REQUESTED">No longer used</option><option value="LOST_OR_STOLEN">Lost or stolen</option><option value="COMPROMISED">Possibly compromised</option><option value="REPLACED">Replaced</option>
        </select></label> : null}
        {error ? <p role="alert" className="text-sm text-destructive">{messagingErrorMessage(error)}</p> : null}
        <DialogFooter>
          <Button variant="outline" disabled={pending} onClick={() => setSelected(null)}>Cancel</Button>
          <Button variant="destructive" disabled={pending} onClick={async () => {
            try {
              if (selected === "sessions") {
                const count = await sessions.mutateAsync();
                setNotice(`Ended ${count} messaging client session${count === 1 ? "" : "s"}. Devices and encrypted history were kept.`);
              }
              else if (selected) { await revoke.mutateAsync({ deviceId: selected.deviceId, reason }); setNotice("The messaging device was revoked."); }
              setSelected(null);
            } catch { /* The mutation error is rendered in the dialog. */ }
          }}>{pending ? "Updating…" : selected === "sessions" ? "End all sessions" : "Revoke device"}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </section>;
}
