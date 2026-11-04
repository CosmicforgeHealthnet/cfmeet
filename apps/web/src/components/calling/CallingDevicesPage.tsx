"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { callingErrorMessage } from "@/lib/calling/errors";
import { endpointsService } from "@/services/calling";
import { useCallingDevice } from "@/hooks/calling/useCallingDevice";
import { useCallingEndpoints, useCallingIdentity } from "@/hooks/calling/useCalls";
import { useMutation, useQueryClient } from "@tanstack/react-query";

export default function CallingDevicesPage() {
  const identity = useCallingIdentity();
  const principalId = identity.data?.principalId;
  const device = useCallingDevice(principalId);
  const endpoints = useCallingEndpoints(!!principalId);
  const queryClient = useQueryClient();
  const [error, setError] = useState("");
  const revoke = useMutation({
    mutationFn: (id: string) => endpointsService.revoke(id),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["calling", "endpoints"] });
    },
  });

  if (identity.isPending) return <div className="p-6"><Spinner /></div>;
  if (identity.error || !principalId) return <div className="p-6"><h1 className="text-2xl font-bold">Calling devices</h1><p role="alert" className="mt-3 text-sm text-destructive">{callingErrorMessage(identity.error)}</p></div>;

  return (
    <div className="mx-auto max-w-5xl space-y-5 p-4 md:p-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Calling devices</h1>
        <p className="text-sm text-muted-foreground">Register this browser as a call endpoint or revoke an endpoint you no longer use.</p>
      </header>
      {error || device.error || revoke.error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error || device.error || callingErrorMessage(revoke.error)}</p> : null}
      <section className="flex flex-wrap items-center justify-between gap-4 rounded-2xl border border-border bg-card p-5">
        <div>
          <h2 className="font-semibold">This browser</h2>
          <p className="text-sm text-muted-foreground" role="status">{device.status === "online" ? "Connected and ready to receive calls" : device.status}</p>
        </div>
        <Button disabled={device.status === "connecting" || device.status === "online"} onClick={() => { setError(""); void device.register(); }}>
          {device.status === "connecting" ? "Connecting…" : "Register this browser"}
        </Button>
      </section>
      <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
        <div className="flex items-center justify-between gap-3"><h2 className="font-semibold">Registered endpoints</h2><Button size="sm" variant="outline" disabled={endpoints.isFetching} onClick={() => void endpoints.refetch()}>Refresh</Button></div>
        {endpoints.isPending ? <Spinner /> : endpoints.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(endpoints.error)}</p> : !endpoints.data?.data.length ? <p className="py-5 text-sm text-muted-foreground">No calling endpoints are registered.</p> : (
          <ul className="divide-y divide-border">
            {endpoints.data.data.map((endpoint) => (
              <li key={endpoint.endpointId} className="flex flex-wrap items-center justify-between gap-3 py-3">
                <div><p className="text-sm font-medium">{endpoint.endpointKind} · {endpoint.status}</p><p className="text-xs text-muted-foreground">Presence: {endpoint.presence} · Last seen: {endpoint.lastSeenAt ? new Date(endpoint.lastSeenAt).toLocaleString() : "Never"}</p></div>
                {endpoint.status === "ACTIVE" ? <Button variant="destructive" size="sm" disabled={revoke.isPending} onClick={async () => {
                  setError("");
                  try { await revoke.mutateAsync(endpoint.endpointId); }
                  catch (cause) { setError(callingErrorMessage(cause)); }
                }}>Revoke</Button> : null}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
