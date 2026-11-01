"use client";

import { useRef, useState, type FormEvent } from "react";
import Link from "next/link";
import { useParams, useSearchParams } from "next/navigation";
import { useResolveSpace } from "@/hooks/v2/useSpaces";
import { profilesService } from "@/services/v2/profiles.service";
import type { RegistrationCreated } from "@/types/v2";
import { Button } from "@/components/ui/button";
import { Field, RequestError } from "./ManagementUI";

type CustomFieldDraft = { key: string; value: string };

export default function PublicRegistrationForm() {
  const params = useParams<{ id: string }>();
  const search = useSearchParams();
  const identifier = params.id;
  const resolved = useResolveSpace(identifier);
  const [displayName, setDisplayName] = useState("");
  const [email, setEmail] = useState("");
  const [organization, setOrganization] = useState("");
  const [customFields, setCustomFields] = useState<CustomFieldDraft[]>([]);
  const [error, setError] = useState<unknown>();
  const [pending, setPending] = useState(false);
  const [result, setResult] = useState<RegistrationCreated | null>(null);
  const [copyState, setCopyState] = useState("");
  const idempotency = useRef<{ fingerprint: string; key: string } | null>(null);
  const space = resolved.data?.meetingSpace;
  const conferenceId = search.get("conferenceId") || undefined;
  const registrationEnabled = space?.meetingType === "WEBINAR" || space?.meetingType === "TOWN_HALL";

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!space) return;
    setPending(true);
    setError(undefined);
    const fields = Object.fromEntries(customFields.filter(item => item.key.trim()).map(item => [item.key.trim(), item.value]));
    const payload = {
      displayName: displayName.trim(),
      ...(conferenceId ? { conferenceId } : {}),
      ...(email.trim() ? { email: email.trim() } : {}),
      ...(organization.trim() ? { organization: organization.trim() } : {}),
      ...(Object.keys(fields).length ? { customFields: fields } : {}),
    };
    const fingerprint = JSON.stringify(payload);
    if (idempotency.current?.fingerprint !== fingerprint) {
      idempotency.current = { fingerprint, key: crypto.randomUUID() };
    }
    try {
      const created = await profilesService.register(space.id, {
        ...payload,
        idempotencyKey: idempotency.current.key,
      });
      setResult(created);
      setError(undefined);
    } catch (cause) {
      setError(cause);
    } finally {
      setPending(false);
    }
  };

  if (resolved.isLoading) return <main className="mx-auto max-w-xl p-6"><p role="status">Loading registration…</p></main>;
  if (resolved.isError) return <main className="mx-auto max-w-xl space-y-4 p-6"><h1 className="text-2xl font-bold">Registration unavailable</h1><RequestError error={resolved.error} retry={() => void resolved.refetch()} /></main>;
  if (!space || !registrationEnabled) return <main className="mx-auto max-w-xl space-y-3 p-6"><h1 className="text-2xl font-bold">Registration unavailable</h1><p className="text-sm text-muted-foreground">Registration is only available for webinar and town-hall meeting spaces.</p></main>;

  if (result) return <main className="mx-auto max-w-xl space-y-5 p-6">
    <h1 className="text-2xl font-bold">Registration submitted</h1>
    <p className="text-sm text-muted-foreground">{result.created ? "Your registration is recorded." : "This request was already registered."} The server applies the meeting space’s current registration requirements and admission policy.</p>
    {result.admissionToken
      ? <div className="space-y-3 rounded-xl border border-primary/40 bg-card p-4">
          <p className="font-medium">Your admission token is shown only once.</p>
          <p className="text-sm text-muted-foreground">It expires {new Date(result.expiresAt).toLocaleString()} and is limited by the organizer’s admission policy.</p>
          <label className="grid gap-2 text-sm font-medium">Admission link
            <input className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" readOnly value={`${typeof window !== "undefined" ? window.location.origin : ""}/invite#${encodeURIComponent(result.admissionToken)}`} />
          </label>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" onClick={async () => {
              const link = `${window.location.origin}/invite#${encodeURIComponent(result.admissionToken!)}`;
              try {
                await navigator.clipboard.writeText(link);
                setCopyState("Admission link copied.");
              } catch {
                setCopyState("Clipboard access is unavailable. Select and copy the admission link above.");
              }
            }}>Copy admission link</Button>
            <Button asChild><Link href={`/invite#${encodeURIComponent(result.admissionToken)}`}>Continue with admission token</Link></Button>
          </div>
          {copyState && <p role="status" className="text-sm text-muted-foreground">{copyState}</p>}
        </div>
      : <p className="rounded-xl border border-border p-4 text-sm">No token was returned for this repeat request. If you did not save your original admission link, contact the organizer.</p>}
  </main>;

  return <main className="mx-auto max-w-xl space-y-6 p-6">
    <header><h1 className="text-2xl font-bold">Register for {space.title || "this event"}</h1><p className="mt-2 text-sm text-muted-foreground">Required fields and approved custom fields are enforced by the meeting space. The form does not assume or bypass the organizer’s policy.</p></header>
    <form className="space-y-4" onSubmit={submit}>
      <Field label="Display name" required maxLength={100} value={displayName} onChange={event => setDisplayName(event.target.value)} />
      {conferenceId && <p className="text-xs text-muted-foreground">Registration is for conference occurrence {conferenceId}.</p>}
      <Field label="Email (if required by the organizer)" type="email" maxLength={320} value={email} onChange={event => setEmail(event.target.value)} />
      <Field label="Organization (if required by the organizer)" maxLength={200} value={organization} onChange={event => setOrganization(event.target.value)} />
      <fieldset className="space-y-3 rounded-lg border border-border p-4">
        <legend className="px-1 text-sm font-medium">Additional fields</legend>
        <p className="text-xs text-muted-foreground">Enter only fields requested by the organizer. The server validates allowed keys and requirements.</p>
        {customFields.map((field, index) => <div key={index} className="grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
          <Field label="Field key" maxLength={80} value={field.key} onChange={event => setCustomFields(items => items.map((item, current) => current === index ? { ...item, key: event.target.value } : item))} />
          <Field label="Value" maxLength={2000} value={field.value} onChange={event => setCustomFields(items => items.map((item, current) => current === index ? { ...item, value: event.target.value } : item))} />
          <Button type="button" variant="outline" className="self-end" onClick={() => setCustomFields(items => items.filter((_, current) => current !== index))}>Remove</Button>
        </div>)}
        <Button type="button" variant="outline" onClick={() => setCustomFields(items => [...items, { key: "", value: "" }])}>Add a field</Button>
      </fieldset>
      {error !== undefined && <RequestError error={error} />}
      <Button className="w-full" type="submit" disabled={pending}>{pending ? "Submitting…" : "Register"}</Button>
    </form>
  </main>;
}
