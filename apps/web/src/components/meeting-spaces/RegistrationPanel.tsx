"use client";

import { useState } from "react";
import Link from "next/link";
import type { MeetingSpaceResponse } from "@/types/v2";
import { useCancelRegistration, useRegistration, useRegistrations } from "@/hooks/v2/useRegistrations";
import { useProfilePolicy } from "@/hooks/v2/useProfilePolicy";
import { ActionDialog, RequestError } from "./ManagementUI";

function RegistrationRow({
  spaceId,
  registrationId,
  status,
  cancel,
}: {
  spaceId: string;
  registrationId: string;
  status: string;
  cancel: (registrationId: string) => Promise<unknown>;
}) {
  const [expanded, setExpanded] = useState(false);
  const detail = useRegistration(spaceId, expanded ? registrationId : "");
  const registration = detail.data;
  return (
    <article className="space-y-3 rounded-xl border border-border bg-card p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <p className="font-medium">{registration?.displayName || "Registrant"} · {status}</p>
          <p className="break-all text-sm text-muted-foreground">
            {registration?.email || "No email provided"} · {registration?.organization || "No organization"}
          </p>
        </div>
        <div className="flex gap-2">
          <button type="button" className="text-sm underline" onClick={() => setExpanded(value => !value)}>
            {expanded ? "Hide details" : "Details"}
          </button>
          {status !== "CANCELLED" && <ActionDialog
            label="Cancel registration"
            destructive
            description="Cancel this registration and revoke its admission token."
            submit={() => cancel(registrationId)}
          />}
        </div>
      </div>
      {expanded && detail.isLoading && <p role="status" className="text-sm">Loading registration…</p>}
      {expanded && detail.isError && <RequestError error={detail.error} retry={() => void detail.refetch()} />}
      {expanded && registration && <dl className="grid gap-2 text-sm sm:grid-cols-2">
        <div><dt className="text-muted-foreground">Registration ID</dt><dd className="break-all">{registration.id}</dd></div>
        <div><dt className="text-muted-foreground">Occurrence</dt><dd>{registration.conferenceId || "Any occurrence"}</dd></div>
        <div><dt className="text-muted-foreground">Created</dt><dd>{new Date(registration.createdAt).toLocaleString()}</dd></div>
        <div><dt className="text-muted-foreground">Cancelled</dt><dd>{registration.cancelledAt ? new Date(registration.cancelledAt).toLocaleString() : "No"}</dd></div>
        {Object.keys(registration.customFields).length > 0 && <div className="sm:col-span-2"><dt className="text-muted-foreground">Additional fields</dt><dd>{Object.entries(registration.customFields).map(([key, value]) => `${key}: ${String(value)}`).join(" · ")}</dd></div>}
      </dl>}
    </article>
  );
}

export default function RegistrationPanel({ space }: { space: MeetingSpaceResponse }) {
  const allowed = ["OWNER", "HOST", "CO_HOST"].includes(space.viewerRole || "");
  const meetingTypeEnabled = space.meetingType === "WEBINAR" || space.meetingType === "TOWN_HALL";
  const policy = useProfilePolicy(space.id, allowed && meetingTypeEnabled);
  const registrationEnabled = policy.data?.effective.policy.registration.enabled === true;
  const list = useRegistrations(space.id, allowed && meetingTypeEnabled && registrationEnabled);
  const cancel = useCancelRegistration(space.id);
  if (!allowed || !meetingTypeEnabled) return null;
  return (
    <section className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold">Registrations</h2>
        <p className="text-sm text-muted-foreground">Webinar and town-hall registrants. Admission tokens are never returned by organizer list or detail APIs.</p>
        {registrationEnabled && <Link className="mt-1 inline-block text-sm text-primary underline" href={`/register/${encodeURIComponent(space.meetingCode)}`}>Open public registration form</Link>}
      </div>
      {policy.isLoading && <p role="status">Loading registration policy…</p>}
      {policy.isError && <RequestError error={policy.error} retry={() => void policy.refetch()} />}
      {policy.data && !registrationEnabled && <p className="text-sm text-muted-foreground">Public registration is disabled by this meeting space’s policy.</p>}
      {list.isLoading && <p role="status">Loading registrations…</p>}
      {list.isError && <RequestError error={list.error} retry={() => void list.refetch()} />}
      {list.data?.registrations.length === 0 && <p className="text-sm text-muted-foreground">No registrations yet.</p>}
      {list.data?.registrations.map(item => (
        <RegistrationRow
          key={item.id}
          spaceId={space.id}
          registrationId={item.id}
          status={item.status}
          cancel={id => cancel.mutateAsync(id)}
        />
      ))}
    </section>
  );
}
