"use client";

/**
 * A status caption with @mentions (WhatsApp): typing "@" lists the people the chosen audience
 * reaches (and only them); picking one inserts "@Name". Deleting the name drops the mention.
 * The server keeps a mention only inside the status's audience snapshot, and only the
 * mentioned person sees "mentioned you".
 */
import { useRef, useState } from "react";
import { cn } from "@/lib/utils";
import type { PersonView } from "@/services/v2/social.service";
import { insertMention, mentionQuery, mentionsInText, mentionsWithinAudience, searchPeople, type Named } from "@/lib/status-privacy";
import { PersonAvatar } from "./shared";

export interface CaptionValue {
  text: string;
  picked: Named[];
}

export const EMPTY_CAPTION: CaptionValue = { text: "", picked: [] };

/** The principals to send as `mentions` for this caption and audience. */
export function captionMentions(value: CaptionValue, members: string[]): string[] {
  return mentionsWithinAudience(mentionsInText(value.text, value.picked), members);
}

export default function MentionCaption({ value, onChange, people, className, placeholder = "Add a caption…", maxLength = 700 }: {
  value: CaptionValue;
  onChange: (v: CaptionValue) => void;
  /** The people the current audience reaches. */
  people: PersonView[];
  className?: string;
  placeholder?: string;
  maxLength?: number;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [caret, setCaret] = useState(0);
  const q = mentionQuery(value.text, caret);
  const matches = q ? searchPeople(people, q.query).slice(0, 6) : [];

  const pick = (p: PersonView) => {
    const next = insertMention(value.text, caret, p.name);
    const id = p.principalId ?? p.userId;
    onChange({ text: next.text.slice(0, maxLength), picked: value.picked.some((x) => x.principalId === id) ? value.picked : [...value.picked, { principalId: id, name: p.name }] });
    setCaret(next.caret);
    requestAnimationFrame(() => { input.current?.focus(); input.current?.setSelectionRange(next.caret, next.caret); });
  };

  return (
    <div className="relative min-w-0 flex-1">
      {q && matches.length ? (
        <ul role="listbox" aria-label="Mention someone" data-testid="mention-list"
          className="absolute bottom-full left-0 z-20 mb-2 max-h-64 w-full max-w-sm overflow-y-auto rounded-2xl bg-background p-1 text-foreground shadow-2xl ring-1 ring-border">
          {matches.map((p) => (
            <li key={p.principalId ?? p.userId}>
              <button type="button" role="option" aria-selected={false} onMouseDown={(e) => e.preventDefault()} onClick={() => pick(p)} data-testid="mention-option"
                className="flex w-full items-center gap-2.5 rounded-xl px-2 py-1.5 text-left text-sm hover:bg-muted">
                <PersonAvatar person={p} size="size-8" />
                <span className="truncate font-medium">{p.name}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      <input ref={input} value={value.text} maxLength={maxLength} placeholder={placeholder} aria-label="Caption" data-testid="status-caption"
        onChange={(e) => { onChange({ ...value, text: e.target.value }); setCaret(e.target.selectionStart ?? e.target.value.length); }}
        onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? 0)}
        onKeyDown={(e) => { if (e.key === "Enter" && q && matches[0]) { e.preventDefault(); pick(matches[0]); } }}
        className={cn("h-12 w-full min-w-0 rounded-full bg-white/10 px-5 text-base text-white placeholder:text-white/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-primary", className)} />
    </div>
  );
}
