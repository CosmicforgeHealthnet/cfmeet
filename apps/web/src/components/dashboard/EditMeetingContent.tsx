"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useSpace, useUpdateSpace } from "@/hooks/v2/useSpaces";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { Spinner } from "@/components/ui/spinner";
import { spacePermissions, spaceUpdate, type SpaceFormValues } from "@/lib/meeting-spaces";
import { getErrorMessage } from "@/utils/error";
import type { MeetingSpaceResponse } from "@/types/v2";

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
  return <form className="max-w-xl space-y-5" onSubmit={async (event) => {
    event.preventDefault(); setError("");
    try { await update.mutateAsync(spaceUpdate(reviewed, values)); router.push("/dashboard/meetings"); }
    catch (cause) { setError(getErrorMessage(cause, "Could not update the meeting space.")); }
  }}>
    <p className="text-sm text-muted-foreground">Edit reusable meeting settings. Conference dates and live-call status are managed separately.</p>
    {changed ? <div role="alert" className="rounded-xl border border-border bg-muted p-3 text-sm">This space changed while you were editing. Reload its settings before saving. <Button type="button" size="sm" variant="outline" onClick={reload}>Reload settings</Button></div> : null}
    <div className="space-y-2"><Label htmlFor="space-title">Title</Label><Input id="space-title" value={values.title} maxLength={255} required onChange={(e) => set("title", e.target.value)} /></div>
    <div className="space-y-2"><Label htmlFor="space-description">Description</Label><Input id="space-description" value={values.description} onChange={(e) => set("description", e.target.value)} /></div>
    <div className="space-y-2"><Label htmlFor="space-type">Meeting type</Label><select id="space-type" className="h-10 w-full rounded-md border border-input bg-background px-3" value={values.meetingType} onChange={(e) => set("meetingType", e.target.value as SpaceFormValues["meetingType"])}>{["STANDARD", "HEALTHCARE", "WEBINAR", "TOWN_HALL", "TRAINING"].map((type) => <option key={type} value={type}>{type.replaceAll("_", " ")}</option>)}</select></div>
    <div className="space-y-2"><Label htmlFor="space-timezone">Timezone</Label><Input id="space-timezone" value={values.timezone} required placeholder="Africa/Lagos" onChange={(e) => set("timezone", e.target.value)} /></div>
    <div className="space-y-2"><Label htmlFor="space-access">Who can request admission?</Label><select id="space-access" className="h-10 w-full rounded-md border border-input bg-background px-3" value={values.accessPolicy} onChange={(e) => set("accessPolicy", e.target.value as SpaceFormValues["accessPolicy"])}>
      <option value="PUBLIC">Public</option><option value="PUBLIC_PASSCODE">Public with passcode</option><option value="AUTHENTICATED">Signed-in users</option><option value="INVITE_ONLY">Members and valid invitations</option>
      {!["PUBLIC", "PUBLIC_PASSCODE", "AUTHENTICATED", "INVITE_ONLY"].includes(reviewed.accessPolicy) ? <option value={reviewed.accessPolicy}>{reviewed.accessPolicy.replaceAll("_", " ")}</option> : null}
    </select><p className="text-xs text-muted-foreground">A meeting link identifies this space. It does not grant membership or a host role.</p></div>
    <div className="space-y-2"><Label htmlFor="space-passcode">New passcode</Label><Input id="space-passcode" type="password" autoComplete="new-password" value={values.passcode} maxLength={64} placeholder={space.requiresPasscode ? "Leave blank to keep the existing passcode" : "Optional unless passcode access is selected"} onChange={(e) => set("passcode", e.target.value)} /></div>
    {space.requiresPasscode ? <label className="flex items-center gap-2 text-sm"><Checkbox checked={values.clearPasscode} onCheckedChange={(checked) => set("clearPasscode", checked === true)} /> Remove existing passcode</label> : null}
    <div className="space-y-2"><Label htmlFor="space-waiting">Waiting room</Label><select id="space-waiting" className="h-10 w-full rounded-md border border-input bg-background px-3" value={values.waitingRoomPolicy} onChange={(e) => set("waitingRoomPolicy", e.target.value as SpaceFormValues["waitingRoomPolicy"])}>{["OFF", "GUESTS_ONLY", "EVERYONE", "UNAUTHENTICATED_ONLY", "NON_MEMBERS_ONLY", "EXTERNAL_ONLY", "CUSTOM"].map((policy) => <option key={policy} value={policy}>{policy.replaceAll("_", " ")}</option>)}</select></div>
    {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
    <div className="flex gap-3"><Button type="button" variant="outline" onClick={() => router.push("/dashboard/meetings")}>Cancel</Button><Button type="submit" disabled={changed || update.isPending}>Save space settings</Button></div>
  </form>;
}

export default function EditMeetingContent({ meetingId }: { meetingId: string }) {
  const space = useSpace(meetingId);
  const [revision, setRevision] = useState(0);
  return <div className="space-y-5 p-4 sm:p-6 md:p-10"><h1 className="text-2xl font-bold">Edit meeting space</h1>
    {space.isPending ? <Spinner /> : space.error || !space.data ? <p role="alert">{getErrorMessage(space.error, "Meeting space not found.")}</p> : !spacePermissions(space.data).configure || space.data.status !== "ACTIVE" ? <p>You need owner or host permission to edit an active meeting space.</p> : <EditSpaceForm key={`${space.data.id}-${revision}`} space={space.data} reload={() => void space.refetch().then(() => setRevision((value) => value + 1))} />}
  </div>;
}
