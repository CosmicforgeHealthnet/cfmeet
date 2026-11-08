"use client";

/**
 * Chat-to-call / contact-to-call launcher (CAL-014) for the Messaging and Contacts screens.
 *
 *   <CallLaunchButton conversationId={id} kind="VIDEO" />   // DIRECT or GROUP conversation
 *   <CallLaunchButton contactId={id} kind="VOICE" />        // accepted contact
 *
 * It uses the Communications Hub launch operations through the app-wide calling session:
 * the Hub applies the communications policy and keeps the timeline call card; ringing, the
 * call controls and media then run in the global call dock (this button holds no call state).
 */

import { useState } from "react";
import { Phone, Video } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useCalling } from "@/context/CallingContext";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { CallKind } from "@/types/calling";

type Target = { contactId: string; conversationId?: never } | { conversationId: string; contactId?: never };

export default function CallLaunchButton({ kind = "VOICE", label, onError, ...target }: Target & {
  kind?: CallKind;
  label?: string;
  /** Receives a user-facing message; without it the message is shown as the button title. */
  onError?: (message: string) => void;
}) {
  const calling = useCalling();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const text = label ?? (kind === "VIDEO" ? "Video call" : "Voice call");
  const unavailable = !calling ? "Calling is not available for this account." : !calling.device.endpoint ? "Turn on receiving calls in this browser (Calls › Devices) to call from here." : calling.media ? "You are already in a call." : null;

  return (
    <Button size="icon" variant="ghost" aria-label={text} title={error ?? unavailable ?? text} disabled={busy || !!unavailable} onClick={async () => {
      if (!calling) return;
      setBusy(true); setError(null);
      try {
        if (target.contactId) await calling.callContact(target.contactId, kind);
        else if (target.conversationId) await calling.callConversation(target.conversationId, kind);
      } catch (e) {
        const message = callingErrorMessage(e);
        setError(message);
        onError?.(message);
      } finally {
        setBusy(false);
      }
    }}>
      {kind === "VIDEO" ? <Video className="size-4" /> : <Phone className="size-4" />}
    </Button>
  );
}
