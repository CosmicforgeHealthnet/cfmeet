"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { Camera, Globe, Lock } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { CATEGORIES, COUNTRIES, handleHint, handleProblem, LANGUAGES, MEDIA_LIMITS, normalizeHandle } from "@/lib/channels";
import { channelsService, type Channel, type ChannelFields, type Visibility } from "@/services/v2/channels.service";
import { errorText } from "@/components/updates/shared";
import { useChannelsRefresh } from "@/hooks/v2/useChannels";
import { ChannelAvatar, Field, inputClass } from "./bits";

const displayName = (type: "language" | "region", code: string) => {
  try { return new Intl.DisplayNames([typeof navigator === "undefined" ? "en" : navigator.language || "en"], { type }).of(code) ?? code; } catch { return code; }
};

/**
 * Create or edit a channel: name, description, unique username (checked as you type),
 * category, language, country, public / private and the picture. Only the owner changes
 * the username or visibility; a verified channel keeps its username.
 */
export default function ChannelForm({ channel }: { channel?: Channel }) {
  const router = useRouter();
  const refresh = useChannelsRefresh();
  const owner = !channel || channel.myRole === "OWNER";
  const [name, setName] = useState(channel?.name ?? "");
  const [description, setDescription] = useState(channel?.description ?? "");
  const [handle, setHandle] = useState(channel?.handle ?? "");
  const [category, setCategory] = useState(channel?.category ?? "");
  const [language, setLanguage] = useState(channel?.language ?? "");
  const [country, setCountry] = useState(channel?.country ?? "");
  const [visibility, setVisibility] = useState<Visibility>(channel?.visibility ?? "PUBLIC");
  const [picture, setPicture] = useState<File | null>(null);
  const [removePicture, setRemovePicture] = useState(false);
  const [remote, setRemote] = useState<{ handle: string; problem: "TAKEN" | "INVALID" | "RESERVED" | null } | null>(null);
  const [busy, setBusy] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const normalized = normalizeHandle(handle);
  const handleChanged = normalized !== (channel?.handle ?? "");
  const lockedHandle = !!channel?.verified;

  const localProblem = normalized && handleChanged ? handleProblem(normalized) : null;
  const needsLookup = !!normalized && handleChanged && !localProblem;
  const handleState = {
    checking: needsLookup && remote?.handle !== normalized,
    problem: localProblem ?? (needsLookup && remote?.handle === normalized ? remote.problem : null),
  };
  useEffect(() => {
    if (!needsLookup) return;
    let live = true;
    const t = setTimeout(() => {
      channelsService.lookup(normalized).then(
        (r) => { if (live) setRemote({ handle: normalized, problem: r.available ? null : (r.reason ?? "TAKEN") }); },
        () => { if (live) setRemote({ handle: normalized, problem: null }); },
      );
    }, 400);
    return () => { live = false; clearTimeout(t); };
  }, [normalized, needsLookup]);

  const save = async () => {
    if (busy || !name.trim() || handleState.problem || handleState.checking) return;
    setBusy(true);
    try {
      const body: ChannelFields = { name: name.trim(), description: description.trim(), category, language, country };
      if (owner) {
        if (handleChanged && !lockedHandle) body.handle = normalized;
        body.visibility = visibility;
      }
      if (picture) {
        if (picture.size > MEDIA_LIMITS.IMAGE) throw new Error("A picture can be at most 8 MB.");
        body.avatarMediaId = (await channelsService.upload(picture, "IMAGE", null)).mediaId;
      } else if (removePicture) {
        body.avatarMediaId = null;
      }
      const saved = channel ? await channelsService.update(channel.channelId, body) : await channelsService.create(body);
      await refresh(saved.channelId);
      toast(channel ? "Saved" : "Channel created");
      router.push(`/dashboard/updates/channels/${saved.channelId}`);
    } catch (e) {
      toast(e instanceof Error && !("response" in e) ? e.message : errorText(e, "The channel couldn't be saved."), "error");
    } finally {
      setBusy(false);
    }
  };

  const preview = useMemo(() => (picture ? URL.createObjectURL(picture) : null), [picture]);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  return (
    <form className="mx-auto max-w-lg space-y-4 px-4 py-6" onSubmit={(e) => { e.preventDefault(); void save(); }} data-testid="channel-form">
      <div className="flex flex-col items-center gap-2">
        <button type="button" onClick={() => picker.current?.click()} className="relative" aria-label="Choose a channel picture">
          {preview ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img src={preview} alt="" className="size-24 rounded-full object-cover" />
          ) : <ChannelAvatar name={name || "Channel"} avatar={removePicture ? null : channel?.avatar} size="size-24" />}
          <span className="absolute bottom-0 right-0 flex size-8 items-center justify-center rounded-full bg-primary text-primary-foreground"><Camera className="size-4" /></span>
        </button>
        <input ref={picker} type="file" accept="image/jpeg,image/png,image/webp" className="hidden" onChange={(e) => { setPicture(e.target.files?.[0] ?? null); setRemovePicture(false); e.target.value = ""; }} />
        {channel?.avatar && !picture && !removePicture ? <button type="button" onClick={() => setRemovePicture(true)} className="text-xs font-semibold text-destructive">Remove picture</button> : null}
      </div>
      <Field label="Channel name">
        <input value={name} onChange={(e) => setName(e.target.value)} maxLength={80} required className={inputClass} placeholder="e.g. Lagos Tech Weekly" />
      </Field>
      <Field label="Description" hint={`${description.length}/500`}>
        <textarea value={description} onChange={(e) => setDescription(e.target.value)} maxLength={500} rows={3} className={cn(inputClass, "h-auto py-2")} placeholder="What is this channel about?" />
      </Field>
      <Field label="Username" hint={lockedHandle ? "A verified channel keeps its username." : !owner ? "Only the owner changes the username." : handleState.checking ? "Checking…" : handleHint(handleState.problem) ?? (normalized && handleChanged ? "Available" : "Optional. People can find the channel by it.")}>
        <div className="flex items-center gap-1 rounded-xl border border-border px-3 focus-within:ring-2 focus-within:ring-primary/40">
          <span className="text-muted-foreground">@</span>
          <input value={handle} onChange={(e) => setHandle(e.target.value)} disabled={!owner || lockedHandle} maxLength={33} autoCapitalize="none" spellCheck={false}
            className="h-11 min-w-0 flex-1 bg-transparent text-[15px] focus:outline-none disabled:opacity-60" placeholder="lagos_tech" data-testid="channel-handle" />
        </div>
      </Field>
      <div className="grid gap-4 sm:grid-cols-3">
        <Field label="Category">
          <select value={category} onChange={(e) => setCategory(e.target.value)} className={inputClass}>
            <option value="">None</option>
            {CATEGORIES.map((c) => <option key={c.code} value={c.code}>{c.label}</option>)}
          </select>
        </Field>
        <Field label="Language">
          <select value={language} onChange={(e) => setLanguage(e.target.value)} className={inputClass}>
            <option value="">None</option>
            {[...new Set([...(language ? [language] : []), ...LANGUAGES])].map((l) => <option key={l} value={l}>{displayName("language", l)}</option>)}
          </select>
        </Field>
        <Field label="Country">
          <select value={country} onChange={(e) => setCountry(e.target.value)} className={inputClass}>
            <option value="">None</option>
            {COUNTRIES.map((c) => <option key={c} value={c}>{displayName("region", c)}</option>)}
          </select>
        </Field>
      </div>
      <fieldset disabled={!owner} className="space-y-2">
        <legend className="mb-1 text-sm font-medium">Who can see it</legend>
        {([["PUBLIC", Globe, "Public", "Anyone can find it, read it and follow it."], ["PRIVATE", Lock, "Private", "Only people with an invite link can join and read it."]] as const).map(([v, Icon, label, text]) => (
          <label key={v} className={cn("flex items-start gap-3 rounded-2xl border px-3 py-3", visibility === v ? "border-primary bg-primary/5" : "border-border")}>
            <input type="radio" name="visibility" className="mt-1 accent-primary" checked={visibility === v} onChange={() => setVisibility(v)} />
            <Icon className="mt-0.5 size-4" />
            <span><span className="block text-sm font-semibold">{label}</span><span className="block text-xs text-muted-foreground">{text}</span></span>
          </label>
        ))}
      </fieldset>
      <button type="submit" disabled={busy || !name.trim() || !!handleState.problem || handleState.checking} className="h-12 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-50" data-testid="channel-save">
        {busy ? "Saving…" : channel ? "Save changes" : "Create channel"}
      </button>
    </form>
  );
}
