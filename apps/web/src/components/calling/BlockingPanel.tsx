"use client";

import { useState } from "react";
import { Ban, Trash2, Plus, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { useBlocks, useCreateBlock, useDeleteBlock } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { BlockReason } from "@/types/calling";

interface BlockingPanelProps {
  className?: string;
}

export default function BlockingPanel({ className }: BlockingPanelProps) {
  const [showForm, setShowForm] = useState(false);
  const [targetKind, setTargetKind] = useState<"PRINCIPAL" | "CONTACT_REFERENCE">("PRINCIPAL");
  const [targetId, setTargetId] = useState("");
  const [reasonCode, setReasonCode] = useState<BlockReason>("UNWANTED");
  const [error, setError] = useState<string | null>(null);

  const blocks = useBlocks();
  const createBlock = useCreateBlock();
  const deleteBlock = useDeleteBlock();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await createBlock.mutateAsync({
        ...(targetKind === "PRINCIPAL"
          ? { blockedPrincipalId: targetId.trim() }
          : { blockedContactReferenceId: targetId.trim() }),
        reasonCode,
      });
      setTargetId("");
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

  const entries = blocks.data?.items ?? [];

  return (
    <div className={className}>
      <div className="mb-4 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Ban className="size-5 text-muted-foreground" />
          <h2 className="text-lg font-semibold">Blocked callers</h2>
        </div>
        <Button
          size="sm"
          variant={showForm ? "outline" : "default"}
          onClick={() => setShowForm(!showForm)}
        >
          {showForm ? "Cancel" : <><Plus className="mr-1 size-4" /> Add block</>}
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
            <Label htmlFor="blocked-target-kind">Caller type</Label>
            <select
              id="blocked-target-kind"
              value={targetKind}
              onChange={(e) => setTargetKind(e.target.value as typeof targetKind)}
              className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
            >
              <option value="PRINCIPAL">CF Meet principal</option>
              <option value="CONTACT_REFERENCE">Protected contact reference</option>
            </select>
          </div>
          <div className="space-y-2">
            <Label htmlFor="blocked-target">Caller ID</Label>
            <Input
              id="blocked-target"
              value={targetId}
              onChange={(e) => setTargetId(e.target.value)}
              placeholder="UUID"
              required
            />
            <p className="text-xs text-muted-foreground">
              Use the principal ID or opaque protected-contact reference ID. Phone numbers are not accepted.
            </p>
          </div>
          <div className="space-y-2">
            <Label htmlFor="block-reason">Reason</Label>
            <select
              id="block-reason"
              value={reasonCode}
              onChange={(e) => setReasonCode(e.target.value as BlockReason)}
              className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
            >
              <option value="UNWANTED">Unwanted</option>
              <option value="HARASSMENT">Harassment</option>
              <option value="SPAM">Spam</option>
              <option value="OTHER">Other</option>
            </select>
          </div>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={() => setShowForm(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={createBlock.isPending}>
              {createBlock.isPending ? <Spinner className="mr-2 size-4" /> : null}
              Add block
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
          No blocked callers. Add a principal or protected contact reference to block.
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
                  <UserRound className="size-5 text-destructive" />
                </div>
                <div>
                  <p className="font-medium">
                    {block.blockedKind === "PRINCIPAL" ? "Principal" : "Protected contact"} ·{" "}
                    {block.blockedPrincipalId ?? block.blockedContactReferenceId ?? "Unknown"}
                  </p>
                  <p className="text-sm text-muted-foreground">
                    {block.status.toLowerCase()} · {block.reasonCode.toLowerCase()}
                  </p>
                </div>
              </div>
              <Button
                size="icon"
                variant="ghost"
                onClick={() => handleDelete(block.blockId)}
                disabled={deleteBlock.isPending || block.status !== "ACTIVE"}
                aria-label="Unblock"
              >
                <Trash2 className="size-4 text-destructive" />
              </Button>
            </li>
          ))}
        </ul>
      )}

    </div>
  );
}
