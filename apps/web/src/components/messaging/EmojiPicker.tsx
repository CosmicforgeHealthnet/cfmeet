"use client";

import { useState } from "react";
import { Popover } from "radix-ui";
import { Smile } from "lucide-react";
import { cn } from "@/lib/utils";

/** The emoji people use most, by WhatsApp's categories (no download, no tracking). */
const CATEGORIES: [string, string[]][] = [
  ["Smileys", ["😀", "😃", "😄", "😁", "😆", "😅", "😂", "🤣", "😊", "😇", "🙂", "🙃", "😉", "😌", "😍", "🥰", "😘", "😗", "😙", "😚", "😋", "😛", "😝", "😜", "🤪", "🤨", "🧐", "🤓", "😎", "🥳", "😏", "😒", "😞", "😔", "😟", "😕", "🙁", "😣", "😖", "😫", "😩", "🥺", "😢", "😭", "😤", "😠", "😡", "🤯", "😳", "🥵", "🥶", "😱", "😨", "😰", "😥", "😓", "🤗", "🤔", "🤭", "🤫", "🤥", "😶", "😐", "😑", "😬", "🙄", "😯", "😦", "😧", "😮", "😲", "🥱", "😴", "🤤", "😪", "😵", "🤐", "🥴", "🤢", "🤮", "🤧", "😷", "🤒", "🤕"]],
  ["Gestures", ["👍", "👎", "👌", "✌️", "🤞", "🤟", "🤘", "🤙", "👈", "👉", "👆", "👇", "☝️", "✋", "🤚", "🖐️", "🖖", "👋", "🤝", "🙏", "👏", "🙌", "👐", "🤲", "💪", "✍️", "🫶", "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "🤎", "💔", "❣️", "💕", "💞", "💓", "💗", "💖", "💘", "💝"]],
  ["Celebrate", ["🎉", "🎊", "🎈", "🎁", "🎂", "🥂", "🍾", "🏆", "🥇", "⭐", "🌟", "✨", "🔥", "💯", "✅", "❌", "⚡", "💥", "🎯", "📌", "📎", "📅", "⏰", "💡", "📣", "🔔", "💬", "👀", "🚀", "🙈", "🙉", "🙊"]],
  ["Food", ["☕", "🍵", "🥤", "🍺", "🍷", "🍕", "🍔", "🍟", "🌭", "🥪", "🌮", "🍝", "🍜", "🍣", "🍚", "🍗", "🥗", "🍰", "🍩", "🍪", "🍫", "🍎", "🍌", "🍉", "🍓", "🥑"]],
  ["Travel", ["🚗", "🚕", "🚌", "🚆", "✈️", "🚢", "🏠", "🏢", "🏥", "🏫", "⛪", "🕌", "🌍", "🗺️", "⛰️", "🏖️", "🌅", "🌙", "☀️", "⛅", "🌧️", "❄️", "🌈", "🌊"]],
];

/** WhatsApp's emoji button in the composer: picks an emoji into the message being written. */
export function EmojiPicker({ onPick, disabled }: { onPick: (emoji: string) => void; disabled?: boolean }) {
  const [category, setCategory] = useState(0);
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button type="button" disabled={disabled} aria-label="Emoji" title="Emoji" data-testid="emoji-button"
          className="flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50 [@media(hover:none)]:hidden">
          <Smile className="size-5" />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content side="top" align="start" sideOffset={8} data-testid="emoji-picker"
          className="z-50 w-[19rem] rounded-2xl border border-border bg-popover p-2 text-popover-foreground shadow-xl">
          <div className="mb-1.5 flex gap-1 overflow-x-auto" role="tablist" aria-label="Emoji categories">
            {CATEGORIES.map(([name], i) => (
              <button key={name} type="button" role="tab" aria-selected={category === i} onClick={() => setCategory(i)}
                className={cn("shrink-0 rounded-full px-2.5 py-1 text-xs font-medium", category === i ? "bg-primary/15 text-primary" : "text-muted-foreground hover:bg-muted")}>
                {name}
              </button>
            ))}
          </div>
          <div className="grid max-h-56 grid-cols-8 gap-0.5 overflow-y-auto" role="tabpanel">
            {CATEGORIES[category][1].map((emoji) => (
              <button key={emoji} type="button" aria-label={emoji} onClick={() => onPick(emoji)}
                className="flex size-8 items-center justify-center rounded-lg text-xl hover:bg-muted">
                {emoji}
              </button>
            ))}
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
