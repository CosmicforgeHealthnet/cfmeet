"use client";

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useCalling } from "@/context/CallingContext";
import { callingErrorMessage } from "@/lib/calling/errors";
import { endpointsService } from "@/services/calling";
import { useCallingEndpoints, useCallingIdentity } from "@/hooks/calling/useCalls";

const statusText: Record<string, string> = {
  online: "Connected and ready to receive calls on every CF Meet page",
  connecting: "Connecting…",
  reconnecting: "Reconnecting…",
  offline: "Offline",
  disabled: "Off: this browser does not ring",
};

export default function CallingDevicesPage() {
  const identity = useCallingIdentity();
  const calling = useCalling();
  const endpoints = useCallingEndpoints(!!calling);
  const queryClient = useQueryClient();
  const [error, setError] = useState("");
  const [permission, setPermission] = useState(() => (typeof Notification === "undefined" ? "unsupported" : Notification.permission));
  const revoke = useMutation({
    mutationFn: (id: string) => endpointsService.revoke(id),
    onSuccess: async () => { await queryClient.invalidateQueries({ queryKey: ["calling", "endpoints"] }); },
  });

  if (identity.isPending) return <div className="p-6"><Spinner /></div>;
  if (identity.error || !calling) return <div className="p-6"><h1 className="text-2xl font-bold">Calling devices</h1><p role="alert" className="mt-3 text-sm text-destructive">{identity.error ? callingErrorMessage(identity.error) : "Calling is starting…"}</p></div>;
  const { device } = calling;

  return (
    <div className="mx-auto max-w-5xl space-y-5 p-4 md:p-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Calling devices</h1>
        <p className="text-sm text-muted-foreground">This browser registers itself automatically after you sign in. Turn it off here, or revoke endpoints you no longer use.</p>
      </header>
      {error || device.error || revoke.error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error || device.error || callingErrorMessage(revoke.error)}</p> : null}
      <section className="space-y-4 rounded-2xl border border-border bg-card p-5">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div>
            <h2 className="font-semibold">Receive calls in this browser</h2>
            <p className="text-sm text-muted-foreground" role="status">{statusText[device.status] ?? device.status}</p>
          </div>
          <Button variant={device.receiveCalls ? "outline" : "default"} disabled={device.status === "connecting"} onClick={() => { setError(""); void device.setReceiving(!device.receiveCalls); }}>
            {device.receiveCalls ? "Turn off" : "Turn on"}
          </Button>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-4 border-t border-border pt-4">
          <div>
            <h3 className="text-sm font-semibold">Background notifications</h3>
            <p className="text-sm text-muted-foreground">
              {permission === "granted" ? "Incoming calls show a system notification while CF Meet is open in a background tab."
                : permission === "denied" ? "Notifications are blocked in your browser settings."
                : permission === "unsupported" ? "This browser does not support notifications."
                : "Allow a system notification for incoming calls while CF Meet is in a background tab."}
            </p>
            <p className="text-xs text-muted-foreground">Calls ring only while a CF Meet tab is open; waking a closed browser (Web Push) is not available yet.</p>
          </div>
          {permission === "default" ? <Button variant="outline" onClick={() => void Notification.requestPermission().then(setPermission)}>Allow notifications</Button> : null}
        </div>
      </section>
      <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
        <div className="flex items-center justify-between gap-3"><h2 className="font-semibold">Registered endpoints</h2><Button size="sm" variant="outline" disabled={endpoints.isFetching} onClick={() => void endpoints.refetch()}>Refresh</Button></div>
        {endpoints.isPending ? <Spinner /> : endpoints.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(endpoints.error)}</p> : !endpoints.data?.data.length ? <p className="py-5 text-sm text-muted-foreground">No calling endpoints are registered.</p> : (
          <ul className="divide-y divide-border">
            {endpoints.data.data.map((endpoint) => (
              <li key={endpoint.endpointId} className="flex flex-wrap items-center justify-between gap-3 py-3">
                <div><p className="text-sm font-medium">{endpoint.endpointKind} · {endpoint.status}{endpoint.endpointId === device.endpoint?.endpointId ? " · this browser" : ""}</p><p className="text-xs text-muted-foreground">Presence: {endpoint.presence} · Last seen: {endpoint.lastSeenAt ? new Date(endpoint.lastSeenAt).toLocaleString() : "Never"}</p></div>
                {endpoint.status === "ACTIVE" ? <Button variant="destructive" size="sm" disabled={revoke.isPending} onClick={async () => {
                  setError("");
                  try {
                    if (endpoint.endpointId === device.endpoint?.endpointId) await device.setReceiving(false);
                    else await revoke.mutateAsync(endpoint.endpointId);
                  } catch (cause) { setError(callingErrorMessage(cause)); }
                }}>Revoke</Button> : null}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
