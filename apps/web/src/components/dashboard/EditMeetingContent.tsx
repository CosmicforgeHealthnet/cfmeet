"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ChevronLeft, RefreshCw } from "lucide-react";
import { useSpace, useUpdateSpace } from "@/hooks/v2/useSpaces";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { NiceSelect } from "@/components/ui/nice-select";
import { spacePermissions, spaceUpdate, type SpaceFormValues } from "@/lib/meeting-spaces";
import { getErrorMessage } from "@/utils/error";
import { TimeZoneField } from "@/components/meetings/ScheduleFields";
import type { MeetingSpaceResponse } from "@/types/v2";

const INPUT = "h-11 rounded-xl px-3 py-0 text-sm sm:h-11 sm:px-3 sm:py-0 sm:text-sm";
const section = "space-y-4 border-t border-border pt-6 first:border-t-0 first:pt-0";
const sectionTitle = "text-sm font-semibold text-foreground";

const WAITING_OPTIONS = [
  { value: "OFF", label: "Off", hint: "Everyone joins straight away" },
  { value: "GUESTS_ONLY", label: "Guests only", hint: "People without an account wait" },
  { value: "EVERYONE", label: "Everyone", hint: "The host lets each person in" },
  { value: "UNAUTHENTICATED_ONLY", label: "Signed-out people", hint: "Anyone not signed in waits" },
  { value: "NON_MEMBERS_ONLY", label: "Non-members", hint: "Only meeting members skip it" },
  { value: "EXTERNAL_ONLY", label: "Outside the organization", hint: "Your organization skips it" },
  { value: "CUSTOM", label: "Custom" },
];

function EditSpaceForm({ space, reload }: { space: MeetingSpaceResponse; reload: () => void }) {
  const router = useRouter();
  const update = useUpdateSpace(space.id);
  // Keep the reviewed version with the draft; a background refetch must never
  // silently turn an outdated draft into a write against the newest version.
  const [reviewed] = useState(space);
  const [values, setValues] = useState<SpaceFormValues>({ title: space.title ?? "", description: space.description ?? "",
    timezone: space.timezone ?? "UTC", meetingType: space.meetingType, accessPolicy: space.accessPolicy,
    waitingRoomPolicy: space.waitingRoomPolicy ?? "OFF", passcode: "", clearPasscode: false });
  const [error, setError] = useState("");
  const changed = reviewed.version !== space.version;
  const set = <K extends keyof SpaceFormValues>(key: K, value: SpaceFormValues[K]) => setValues((old) => ({ ...old, [key]: value }));
  const back = `/dashboard/meetings/${space.id}`;

  return (
    <form
      className="mt-6 max-w-3xl rounded-3xl border border-border bg-card p-5 sm:p-8"
      onSubmit={async (event) => {
        event.preventDefault(); setError("");
        try { await update.mutateAsync(spaceUpdate(reviewed, values)); router.push(back); }
        catch (cause) { setError(getErrorMessage(cause, "Could not save your changes.")); }
      }}
    >
      {changed ? (
        <div role="alert" className="mb-6 flex flex-wrap items-center gap-3 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-foreground">
          <span className="flex-1">Someone else changed this meeting while you were editing. Reload to see the latest before saving.</span>
          <Button type="button" variant="outline" className="h-9 rounded-xl" onClick={reload}><RefreshCw className="size-4" /> Reload</Button>
        </div>
      ) : null}

      <div className="space-y-6">
        <div className={section}>
          <p className={sectionTitle}>What</p>
          <div className="grid gap-2"><Label htmlFor="space-title">Meeting name</Label><Input id="space-title" className={INPUT} value={values.title} maxLength={255} required onChange={(e) => set("title", e.target.value)} /></div>
          <label className="grid gap-2 text-sm font-medium text-foreground">
            Description <span className="-mt-1 text-xs font-normal text-muted-foreground">Optional</span>
            <textarea value={values.description} rows={3} placeholder="Agenda, links or notes for attendees" onChange={(e) => set("description", e.target.value)} className="min-h-[88px] rounded-xl border border-border bg-background px-3 py-2.5 text-sm text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary" />
          </label>
          <NiceSelect label="Meeting type" value={values.meetingType} onChange={(v) => set("meetingType", v as SpaceFormValues["meetingType"])} options={[{ value: "STANDARD", label: "Meeting", hint: "Everyone can talk and share" }, { value: "WEBINAR", label: "Webinar", hint: "Presenters speak, the audience watches" }, { value: "TOWN_HALL", label: "Town hall", hint: "Large all-hands with Q&A" }, { value: "TRAINING", label: "Training", hint: "Teach a group with polls and breakouts" }, { value: "HEALTHCARE", label: "Healthcare", hint: "Appointment with stricter privacy" }]} />
        </div>

        <div className={section}>
          <p className={sectionTitle}>Time zone</p>
          <TimeZoneField id="space-timezone" label="Meeting time zone" value={values.timezone} onChange={(zone) => set("timezone", zone)} />
          <p className="-mt-2 text-xs text-muted-foreground">A repeating series keeps the same clock time in the new zone. Change dates from the meeting&apos;s Sessions and Schedule tabs.</p>
        </div>

        <div className={section}>
          <p className={sectionTitle}>Who can join</p>
          <NiceSelect label="Access" value={values.accessPolicy} onChange={(v) => set("accessPolicy", v as SpaceFormValues["accessPolicy"])} options={[{ value: "PUBLIC", label: "Anyone with the link", hint: "Easiest for guests" }, { value: "PUBLIC_PASSCODE", label: "Link + passcode", hint: "Guests also need a code" }, { value: "AUTHENTICATED", label: "Signed-in users", hint: "A CF Meet account is required" }, { value: "INVITE_ONLY", label: "Invite only", hint: "Members and invited people" }, ...(reviewed.organizationId ? [{ value: "ORGANIZATION_ONLY", label: "Organization only", hint: "Members of your organization" }] : []), ...(!["PUBLIC", "PUBLIC_PASSCODE", "AUTHENTICATED", "INVITE_ONLY", ...(reviewed.organizationId ? ["ORGANIZATION_ONLY"] : [])].includes(reviewed.accessPolicy) ? [{ value: reviewed.accessPolicy, label: reviewed.accessPolicy.replaceAll("_", " ").toLowerCase() }] : [])]} />
          <div className="grid gap-2"><Label htmlFor="space-passcode">{space.requiresPasscode ? "Change passcode" : "Passcode"}</Label><Input id="space-passcode" className={INPUT} type="password" autoComplete="new-password" value={values.passcode} maxLength={64} placeholder={space.requiresPasscode ? "Leave blank to keep the current one" : "Needed only for Link + passcode"} onChange={(e) => set("passcode", e.target.value)} /></div>
          {space.requiresPasscode ? <label className="flex items-center gap-2 text-sm text-foreground"><Checkbox checked={values.clearPasscode} onCheckedChange={(checked) => set("clearPasscode", checked === true)} /> Remove the passcode</label> : null}
        </div>

        <div className={section}>
          <p className={sectionTitle}>Options</p>
          <NiceSelect label="Waiting room" value={values.waitingRoomPolicy} onChange={(v) => set("waitingRoomPolicy", v as SpaceFormValues["waitingRoomPolicy"])} options={WAITING_OPTIONS} />
        </div>

        {error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</p> : null}

        <div className="flex flex-col-reverse gap-2 border-t border-border pt-6 sm:flex-row sm:justify-end">
          <Button type="button" variant="outline" className="h-11 rounded-xl" asChild><Link href={back}>Cancel</Link></Button>
          <Button type="submit" className="h-11 rounded-xl px-6 shadow-none hover:brightness-100" disabled={changed || update.isPending}>{update.isPending ? "Saving…" : "Save changes"}</Button>
        </div>
      </div>
    </form>
  );
}

export default function EditMeetingContent({ meetingId }: { meetingId: string }) {
  const space = useSpace(meetingId);
  const [revision, setRevision] = useState(0);
  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <Link href={`/dashboard/meetings/${meetingId}`} className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
        <ChevronLeft className="size-4" /> {space.data?.title || "Meeting"}
      </Link>
      <h1 className="mt-3 text-xl font-bold tracking-tight text-foreground sm:text-3xl">Edit meeting</h1>
      <p className="mt-1 text-sm text-muted-foreground">Change the name, access and options. The link stays the same.</p>

      {space.isPending ? (
        <div className="mt-6 h-96 max-w-3xl animate-pulse rounded-3xl bg-muted" aria-busy="true" />
      ) : space.error || !space.data ? (
        <p role="alert" className="mt-6 rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">{getErrorMessage(space.error, "Meeting not found.")}</p>
      ) : !spacePermissions(space.data).configure || space.data.status !== "ACTIVE" ? (
        <p className="mt-6 rounded-2xl border border-border bg-card p-6 text-sm text-muted-foreground">Only the owner or a host can edit an active meeting.</p>
      ) : (
        <EditSpaceForm key={`${space.data.id}-${revision}`} space={space.data} reload={() => void space.refetch().then(() => setRevision((value) => value + 1))} />
      )}
    </div>
  );
}
