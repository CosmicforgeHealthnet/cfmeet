"use client";

/**
 * VoicemailInbox - List and manage voicemail messages.
 */

import { useState } from "react";
import { Voicemail, Phone, Trash2, Mail, MailOpen, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import {
  useVoicemailMailboxes,
  useVoicemailMessages,
  useDeleteVoicemailMessage,
  useSetReadState,
} from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import VoicemailPlayer from "./VoicemailPlayer";
import type { VoicemailMessage } from "@/types/calling";

interface VoicemailInboxProps {
  className?: string;
}

export default function VoicemailInbox({ className }: VoicemailInboxProps) {
  const [selectedMailboxId, setSelectedMailboxId] = useState<string>("");
  const [selectedMessage, setSelectedMessage] = useState<VoicemailMessage | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mailboxes = useVoicemailMailboxes();
  const messages = useVoicemailMessages({
    mailboxId: selectedMailboxId || mailboxes.data?.data[0]?.mailboxId || "",
  });
  const deleteMessage = useDeleteVoicemailMessage(selectedMailboxId);
  const setReadState = useSetReadState(selectedMailboxId);

  // Auto-select first mailbox
  const activeMailboxId = selectedMailboxId || mailboxes.data?.data[0]?.mailboxId || "";
  const activeMailbox = mailboxes.data?.data.find((m) => m.mailboxId === activeMailboxId);

  const entries = messages.data?.pages.flatMap((page) => page.data) ?? [];
  const unreadCount = messages.data?.pages[0]?.unreadCount ?? 0;

  const handleSelectMessage = async (message: VoicemailMessage) => {
    setSelectedMessage(message);
    // Mark as read when selected
    if (message.readState === "UNREAD") {
      try {
        await setReadState.mutateAsync({
          messageId: message.messageId,
          body: { readState: "READ" },
        });
      } catch {
        // Ignore read state errors
      }
    }
  };

  const handleDelete = async (messageId: string) => {
    setError(null);
    try {
      await deleteMessage.mutateAsync(messageId);
      if (selectedMessage?.messageId === messageId) {
        setSelectedMessage(null);
      }
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const handleToggleRead = async (message: VoicemailMessage) => {
    setError(null);
    try {
      await setReadState.mutateAsync({
        messageId: message.messageId,
        body: { readState: message.readState === "READ" ? "UNREAD" : "READ" },
      });
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const formatDuration = (seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return mins > 0 ? `${mins}:${secs.toString().padStart(2, "0")}` : `${secs}s`;
  };

  return (
    <div className={className}>
      <div className="mb-4 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Voicemail className="size-5 text-muted-foreground" />
          <h2 className="text-lg font-semibold">Voicemail</h2>
          {unreadCount > 0 && (
            <span className="rounded-full bg-primary px-2 py-0.5 text-xs font-medium text-primary-foreground">
              {unreadCount} new
            </span>
          )}
        </div>
        <Button
          size="icon"
          variant="ghost"
          onClick={() => void messages.refetch()}
          disabled={messages.isFetching}
          aria-label="Refresh"
        >
          <RefreshCw className={`size-4 ${messages.isFetching ? "animate-spin" : ""}`} />
        </Button>
      </div>

      {/* Mailbox selector */}
      {mailboxes.data && mailboxes.data.data.length > 1 && (
        <div className="mb-4">
          <select
            value={activeMailboxId}
            onChange={(e) => {
              setSelectedMailboxId(e.target.value);
              setSelectedMessage(null);
            }}
            className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
          >
            {mailboxes.data.data.map((mailbox) => (
              <option key={mailbox.mailboxId} value={mailbox.mailboxId}>
                {mailbox.name}
                {mailbox.extension && ` (ext. ${mailbox.extension})`}
              </option>
            ))}
          </select>
        </div>
      )}

      {error && (
        <p className="mb-4 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
          {error}
        </p>
      )}

      <div className="grid gap-4 lg:grid-cols-2">
        {/* Message list */}
        <div className="space-y-2">
          {mailboxes.isPending || messages.isPending ? (
            <div className="flex items-center justify-center py-8">
              <Spinner />
            </div>
          ) : messages.error ? (
            <p className="text-sm text-destructive">{callingErrorMessage(messages.error)}</p>
          ) : entries.length === 0 ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              No voicemail messages. When someone leaves you a message, it will appear here.
            </p>
          ) : (
            <ul className="space-y-1">
              {entries.map((message) => (
                <li key={message.messageId}>
                  <button
                    onClick={() => handleSelectMessage(message)}
                    aria-pressed={selectedMessage?.messageId === message.messageId}
                    className={`flex w-full items-center gap-3 rounded-xl p-3 text-left hover:bg-muted ${
                      selectedMessage?.messageId === message.messageId ? "bg-muted" : ""
                    } ${message.readState === "UNREAD" ? "font-medium" : ""}`}
                  >
                    <div className="flex size-10 items-center justify-center rounded-full bg-primary/10">
                      <Phone className="size-5 text-primary" />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center justify-between gap-2">
                        <p className="truncate">
                          {message.callerDisplayName || message.callerNumber || "Unknown"}
                        </p>
                        {message.readState === "UNREAD" && (
                          <span className="size-2 shrink-0 rounded-full bg-primary" />
                        )}
                      </div>
                      <p className="text-xs text-muted-foreground">
                        {formatDuration(message.durationSeconds)} ·{" "}
                        {new Date(message.createdAt).toLocaleDateString()}
                      </p>
                    </div>
                    <div className="flex shrink-0 gap-1">
                      <Button
                        size="icon"
                        variant="ghost"
                        onClick={(e) => {
                          e.stopPropagation();
                          handleToggleRead(message);
                        }}
                        aria-label={message.readState === "READ" ? "Mark unread" : "Mark read"}
                      >
                        {message.readState === "READ" ? (
                          <Mail className="size-4" />
                        ) : (
                          <MailOpen className="size-4" />
                        )}
                      </Button>
                      <Button
                        size="icon"
                        variant="ghost"
                        onClick={(e) => {
                          e.stopPropagation();
                          handleDelete(message.messageId);
                        }}
                        disabled={deleteMessage.isPending}
                        aria-label="Delete"
                      >
                        <Trash2 className="size-4 text-destructive" />
                      </Button>
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}

          {messages.hasNextPage && (
            <Button
              variant="ghost"
              className="w-full"
              disabled={messages.isFetchingNextPage}
              onClick={() => void messages.fetchNextPage()}
            >
              Load more
            </Button>
          )}
        </div>

        {/* Player */}
        <div>
          {selectedMessage ? (
            <VoicemailPlayer
              messageId={selectedMessage.messageId}
              onClose={() => setSelectedMessage(null)}
            />
          ) : (
            <div className="flex items-center justify-center rounded-xl border border-dashed border-border py-12 text-sm text-muted-foreground">
              Select a message to play
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
