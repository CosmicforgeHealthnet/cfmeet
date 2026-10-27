"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCreateSpace } from "@/hooks/v2/useSpaces";
import type { AccessPolicy, MeetingType, WaitingRoomPolicy } from "@/types/v2";
import { Button } from "@/components/ui/button";
import { Field, RequestError } from "./ManagementUI";

export default function CreateMeetingSpaceContent() {
  const router = useRouter();
  const create = useCreateSpace();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [timezone, setTimezone] = useState(() => Intl.DateTimeFormat().resolvedOptions().timeZone);
  const [type, setType] = useState<MeetingType>("STANDARD");
  const [access, setAccess] = useState<AccessPolicy>("PUBLIC");
  const [waiting, setWaiting] = useState<WaitingRoomPolicy>("OFF");
  const [passcode, setPasscode] = useState("");
  const [error, setError] = useState<unknown>();
  return <div className="space-y-6 px-4 py-6 sm:p-8 md:p-10"><Button variant="outline" asChild><Link href="/dashboard/meetings">Back to meetings</Link></Button><header><h1 className="text-2xl font-bold">Create meeting space</h1><p className="mt-1 text-sm text-muted-foreground">Save reusable settings and access policies. Schedule conference sessions separately from Conferences.</p></header>
    <form className="max-w-xl space-y-4" onSubmit={async event => {
      event.preventDefault(); setError(undefined);
      try {
        new Intl.DateTimeFormat("en", { timeZone: timezone }).format();
        const space = await create.mutateAsync({ title: title.trim(), description: description.trim() || undefined, timezone, meetingType: type, accessPolicy: access, waitingRoomPolicy: waiting, ...(access === "PUBLIC_PASSCODE" ? { passcode: passcode.trim() } : {}) });
        router.push(`/dashboard/meetings/${space.id}`);
      } catch (cause) { setError(cause); }
    }}><fieldset disabled={create.isPending} className="space-y-4">
      <Field label="Meeting name" value={title} required maxLength={255} onChange={e => setTitle(e.target.value)} />
      <Field label="Description (optional)" value={description} onChange={e => setDescription(e.target.value)} />
      <Field label="Meeting timezone" value={timezone} required placeholder="Africa/Lagos" onChange={e => setTimezone(e.target.value)} />
      <label className="grid gap-2 text-sm font-medium">Meeting type<select className="h-10 rounded-md border border-input bg-background px-3" value={type} onChange={e => setType(e.target.value as MeetingType)}>{["STANDARD", "WEBINAR", "TOWN_HALL", "TRAINING"].map(value => <option key={value} value={value}>{value.replaceAll("_", " ")}</option>)}</select></label>
      <label className="grid gap-2 text-sm font-medium">Access policy<select className="h-10 rounded-md border border-input bg-background px-3" value={access} onChange={e => setAccess(e.target.value as AccessPolicy)}><option value="PUBLIC">Public</option><option value="PUBLIC_PASSCODE">Passcode protected</option><option value="AUTHENTICATED">Signed-in users</option><option value="INVITE_ONLY">Members and invitations</option></select></label>
      {access === "PUBLIC_PASSCODE" && <Field label="Passcode" type="password" minLength={4} maxLength={64} required value={passcode} onChange={e => setPasscode(e.target.value)} />}
      <label className="grid gap-2 text-sm font-medium">Waiting room<select className="h-10 rounded-md border border-input bg-background px-3" value={waiting} onChange={e => setWaiting(e.target.value as WaitingRoomPolicy)}><option value="OFF">Off</option><option value="GUESTS_ONLY">Guests only</option><option value="EVERYONE">Everyone</option></select></label>
      <Button disabled={create.isPending || !title.trim()} type="submit">{create.isPending ? "Creating…" : "Create meeting space"}</Button>
    </fieldset>{error !== undefined && <RequestError error={error} />}</form>
  </div>;
}
