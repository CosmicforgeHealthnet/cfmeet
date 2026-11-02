"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCreateSpace } from "@/hooks/v2/useSpaces";
import type { AccessPolicy, MeetingType, WaitingRoomPolicy } from "@/types/v2";
import { Button } from "@/components/ui/button";
import { Field, RequestError } from "./ManagementUI";

const meetingTypes: MeetingType[] = ["STANDARD", "WEBINAR", "TOWN_HALL", "TRAINING"];

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

  return (
    <div className="mx-auto max-w-6xl px-4 py-6 sm:px-6 sm:py-8 md:px-10 md:py-10">
      <div className="mb-6 flex items-center justify-between gap-3">
        <Button variant="outline" asChild>
          <Link href="/dashboard/meetings">Back to meetings</Link>
        </Button>
      </div>

      <div className="grid gap-6 lg:grid-cols-[1.25fr_0.75fr]">
        <section className="rounded-2xl border border-border bg-card p-5 shadow-sm sm:p-6">
          <header className="mb-6">
            <p className="mb-2 text-xs font-medium uppercase tracking-[0.18em] text-primary">Setup</p>
            <h1 className="text-2xl font-bold tracking-tight text-foreground sm:text-3xl">Create meeting space</h1>
            <p className="mt-2 max-w-xl text-sm text-muted-foreground">
              Save reusable settings and access policies. Schedule conference sessions separately from conferences.
            </p>
          </header>

          <form
            className="space-y-5"
            onSubmit={async event => {
              event.preventDefault();
              setError(undefined);
              try {
                new Intl.DateTimeFormat("en", { timeZone: timezone }).format();
                const space = await create.mutateAsync({
                  title: title.trim(),
                  description: description.trim() || undefined,
                  timezone,
                  meetingType: type,
                  accessPolicy: access,
                  waitingRoomPolicy: waiting,
                  ...(access === "PUBLIC_PASSCODE" ? { passcode: passcode.trim() } : {}),
                });
                router.push(`/dashboard/meetings/${space.id}`);
              } catch (cause) {
                setError(cause);
              }
            }}
          >
            <fieldset disabled={create.isPending} className="space-y-5">
              <div className="grid gap-5 md:grid-cols-2">
                <Field
                  label="Meeting name"
                  value={title}
                  required
                  maxLength={255}
                  placeholder="Weekly project sync"
                  onChange={e => setTitle(e.target.value)}
                />
                <Field
                  label="Meeting timezone"
                  value={timezone}
                  required
                  placeholder="Africa/Lagos"
                  onChange={e => setTimezone(e.target.value)}
                />
              </div>

              <label className="grid gap-2 text-sm font-medium text-foreground">
                Description
                <textarea
                  value={description}
                  rows={4}
                  placeholder="Use this space to host recurring updates, team syncs, and customer sessions."
                  onChange={e => setDescription(e.target.value)}
                  className="min-h-[100px] rounded-md border border-input bg-background px-3 py-2 text-sm text-foreground shadow-sm transition-colors placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
                />
              </label>

              <div className="grid gap-5 md:grid-cols-2">
                <label className="grid gap-2 text-sm font-medium text-foreground">
                  Meeting type
                  <select
                    className="h-10 rounded-md border border-input bg-background px-3 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
                    value={type}
                    onChange={e => setType(e.target.value as MeetingType)}
                  >
                    {meetingTypes.map(value => (
                      <option key={value} value={value}>{value.replaceAll("_", " ")}</option>
                    ))}
                  </select>
                </label>

                <label className="grid gap-2 text-sm font-medium text-foreground">
                  Waiting room
                  <select
                    className="h-10 rounded-md border border-input bg-background px-3 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
                    value={waiting}
                    onChange={e => setWaiting(e.target.value as WaitingRoomPolicy)}
                  >
                    <option value="OFF">Off</option>
                    <option value="GUESTS_ONLY">Guests only</option>
                    <option value="EVERYONE">Everyone</option>
                  </select>
                </label>
              </div>

              <label className="grid gap-2 text-sm font-medium text-foreground">
                Access policy
                <select
                  className="h-10 rounded-md border border-input bg-background px-3 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
                  value={access}
                  onChange={e => setAccess(e.target.value as AccessPolicy)}
                >
                  <option value="PUBLIC">Public</option>
                  <option value="PUBLIC_PASSCODE">Passcode protected</option>
                  <option value="AUTHENTICATED">Signed-in users</option>
                  <option value="INVITE_ONLY">Members and invitations</option>
                </select>
              </label>

              {access === "PUBLIC_PASSCODE" && (
                <Field
                  label="Passcode"
                  type="password"
                  minLength={4}
                  maxLength={64}
                  required
                  value={passcode}
                  placeholder="Enter a secure passcode"
                  onChange={e => setPasscode(e.target.value)}
                />
              )}

              <div className="flex flex-col gap-3 border-t border-border pt-4 sm:flex-row sm:items-center sm:justify-between">
                <p className="text-sm text-muted-foreground">
                  {title.trim() ? `Ready to create “${title.trim()}”` : "Add a meeting name to continue"}
                </p>
                <Button disabled={create.isPending || !title.trim()} type="submit" className="min-w-[180px]">
                  {create.isPending ? "Creating…" : "Create meeting space"}
                </Button>
              </div>
            </fieldset>

            {error !== undefined && <RequestError error={error} />}
          </form>
        </section>

        <aside className="rounded-2xl border border-border bg-gradient-to-b from-primary/5 via-background to-background p-5 shadow-sm sm:p-6">
          <p className="text-xs font-medium uppercase tracking-[0.18em] text-primary">Summary</p>
          <div className="mt-5 space-y-4">
            <div className="rounded-xl border border-border bg-background/80 p-4">
              <p className="text-sm text-muted-foreground">Meeting type</p>
              <p className="mt-2 text-lg font-semibold text-foreground">{type.replaceAll("_", " ")}</p>
            </div>
            <div className="rounded-xl border border-border bg-background/80 p-4">
              <p className="text-sm text-muted-foreground">Access policy</p>
              <p className="mt-2 text-lg font-semibold text-foreground">{access.replaceAll("_", " ")}</p>
            </div>
            <div className="rounded-xl border border-border bg-background/80 p-4">
              <p className="text-sm text-muted-foreground">Waiting room</p>
              <p className="mt-2 text-lg font-semibold text-foreground">{waiting.replaceAll("_", " ")}</p>
            </div>
          </div>

          <div className="mt-6 rounded-xl border border-dashed border-primary/30 bg-primary/5 p-4 text-sm text-muted-foreground">
            This reusable space defines the base meeting configuration. Scheduling, attendee management, and live conference controls are configured from the space detail screen after creation.
          </div>
        </aside>
      </div>
    </div>
  );
}
