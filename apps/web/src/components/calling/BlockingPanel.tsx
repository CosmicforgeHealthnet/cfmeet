"use client";

/**
 * BlockingPanel - Manage blocked numbers and contacts.
 */

import { useState } from "react";
import { Ban, Trash2, Plus, Phone } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { useBlocks, useCreateBlock, useDeleteBlock } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { BlockScope, BlockReason } from "@/types/calling";

interface BlockingPanelProps {
  className?: string;
}

export default function BlockingPanel({ className }: BlockingPanelProps) {
  const [showForm, setShowForm] = useState(false);
  const [blockedNumber, setBlockedNumber] = useState("");
  const [scope, setScope] = useState<BlockScope>("ALL");
  const [reason, setReason] = useState<BlockReason>("UNWANTED");
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);

  const blocks = useBlocks();
  const createBlock = useCreateBlock();
  const deleteBlock = useDeleteBlock();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await createBlock.mutateAsync({
        blockedNumber: blockedNumber.trim() || undefined,
        scope,
        reason,
        note: note.trim() || undefined,
      });
      setBlockedNumber("");
      setNote("");
      setShowForm(false);
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const handleDelete = async (blockId: string) => {
    setError(null);
    try {
      await deleteBlock.mutateAsync(blockId);
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const entries = blocks.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <div className={className}>
      <div className="mb-4 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Ban className="size-5 text-muted-foreground" />
          <h2 className="text-lg font-semibold">Blocked Numbers</h2>
        </div>
        <Button
          size="sm"
          variant={showForm ? "outline" : "default"}
          onClick={() => setShowForm(!showForm)}
        >
          {showForm ? "Cancel" : <><Plus className="mr-1 size-4" /> Block Number</>}
        </Button>
      </div>

      {error && (
        <p className="mb-4 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
          {error}
        </p>
      )}

      {showForm && (
        <form onSubmit={handleSubmit} className="mb-6 space-y-4 rounded-xl border border-border bg-muted/30 p-4">
          <div className="space-y-2">
            <Label htmlFor="blocked-number">Phone Number</Label>
            <Input
              id="blocked-number"
              value={blockedNumber}
              onChange={(e) => setBlockedNumber(e.target.value)}
              placeholder="+1 555 123 4567"
              required
            />
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="block-scope">Block Type</Label>
              <select
                id="block-scope"
                value={scope}
                onChange={(e) => setScope(e.target.value as BlockScope)}
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
              >
                <option value="ALL">All Calls</option>
                <option value="VOICE">Voice Only</option>
                <option value="VIDEO">Video Only</option>
              </select>
            </div>
            <div className="space-y-2">
              <Label htmlFor="block-reason">Reason</Label>
              <select
                id="block-reason"
                value={reason}
                onChange={(e) => setReason(e.target.value as BlockReason)}
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
              >
                <option value="UNWANTED">Unwanted</option>
                <option value="SPAM">Spam</option>
                <option value="ABUSE">Abuse</option>
                <option value="OTHER">Other</option>
              </select>
            </div>
          </div>
          <div className="space-y-2">
            <Label htmlFor="block-note">Note (optional)</Label>
            <Input
              id="block-note"
              value={note}
              onChange={(e) => setNote(e.target.value)}
              placeholder="Additional details"
            />
          </div>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={() => setShowForm(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={createBlock.isPending}>
              {createBlock.isPending ? <Spinner className="mr-2 size-4" /> : null}
              Block Number
            </Button>
          </div>
        </form>
      )}

      {blocks.isPending ? (
        <div className="flex items-center justify-center py-8">
          <Spinner />
        </div>
      ) : blocks.error ? (
        <p className="text-sm text-destructive">{callingErrorMessage(blocks.error)}</p>
      ) : entries.length === 0 ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          No blocked numbers. You can block unwanted callers using the button above.
        </p>
      ) : (
        <ul className="space-y-2">
          {entries.map((block) => (
            <li
              key={block.blockId}
              className="flex items-center justify-between gap-3 rounded-xl border border-border bg-card p-4"
            >
              <div className="flex items-center gap-3">
                <div className="flex size-10 items-center justify-center rounded-full bg-destructive/10">
                  <Phone className="size-5 text-destructive" />
                </div>
                <div>
                  <p className="font-medium">{block.blockedNumber || "Unknown"}</p>
                  <p className="text-sm text-muted-foreground">
                    {block.scope} · {block.reason?.toLowerCase() || "blocked"}
                    {block.note && ` · ${block.note}`}
                  </p>
                </div>
              </div>
              <Button
                size="icon"
                variant="ghost"
                onClick={() => handleDelete(block.blockId)}
                disabled={deleteBlock.isPending}
                aria-label="Unblock"
              >
                <Trash2 className="size-4 text-destructive" />
              </Button>
            </li>
          ))}
        </ul>
      )}

      {blocks.hasNextPage && (
        <Button
          variant="ghost"
          className="mt-4 w-full"
          disabled={blocks.isFetchingNextPage}
          onClick={() => void blocks.fetchNextPage()}
        >
          Load more
        </Button>
      )}
    </div>
  );
}
