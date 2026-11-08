"use client";

import { useState } from "react";
import { Trash2 } from "lucide-react";
import { useWebhooks, useDeleteWebhook } from "@/hooks";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import type { Webhook } from "@/types/webhook";
import { WEBHOOK_EVENT_TYPES } from "@/types/webhook";
import LegacyDeprecationNotice from "./LegacyDeprecationNotice";

/**
 * Legacy meeting webhooks (deprecated, PLT-003): stored by the meeting service but never
 * delivered. Read-only list plus delete (the migration's clean-up step); new subscriptions
 * are Developer Platform webhooks in the developer console (`/developers/webhooks`).
 */
export default function WebhooksContent() {
  const { data: webhooks, isLoading, isError } = useWebhooks();
  const deleteWebhook = useDeleteWebhook();
  const [confirmId, setConfirmId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="flex h-40 items-center justify-center">
        <Spinner />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="bg-card p-6 rounded-2xl shadow-sm border border-border space-y-6">
        <div>
          <h2 className="text-lg font-semibold mb-1">Legacy webhooks</h2>
          <p className="text-sm text-muted-foreground">Registrations made with the old meeting API. No events are sent to them.</p>
        </div>

        <LegacyDeprecationNotice kind="webhooks" />

        {isError ? (
          <p className="text-sm text-red-500">Failed to load legacy webhooks.</p>
        ) : !webhooks || webhooks.length === 0 ? (
          <p className="text-center py-6 text-sm text-muted-foreground">No legacy webhook registrations.</p>
        ) : (
          <div className="space-y-3">
            {webhooks.map((webhook) => (
              <WebhookRow
                key={webhook.id}
                webhook={webhook}
                confirming={confirmId === webhook.id}
                deleting={deleteWebhook.isPending && deleteWebhook.variables === webhook.id}
                onAsk={() => setConfirmId(webhook.id)}
                onCancel={() => setConfirmId(null)}
                onDelete={() => deleteWebhook.mutate(webhook.id, { onSuccess: () => setConfirmId(null) })}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function WebhookRow({
  webhook,
  confirming,
  deleting,
  onAsk,
  onCancel,
  onDelete,
}: {
  webhook: Webhook;
  confirming: boolean;
  deleting: boolean;
  onAsk: () => void;
  onCancel: () => void;
  onDelete: () => void;
}) {
  const eventLabel = WEBHOOK_EVENT_TYPES.find((t) => t.value === webhook.event_type)?.label || webhook.event_type;
  return (
    <div className="p-4 border border-border rounded-xl bg-muted/50">
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0 space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="px-2 py-0.5 rounded-full text-xs font-medium bg-muted text-muted-foreground">{webhook.status}</span>
            <span className="px-2 py-0.5 rounded-full text-xs font-medium bg-blue-100 text-blue-700">{eventLabel}</span>
            <span className="px-2 py-0.5 rounded-full text-xs font-medium bg-amber-100 text-amber-800">not delivered</span>
          </div>
          <p className="text-sm font-medium text-foreground truncate">{webhook.endpoint_url}</p>
          <p className="text-xs text-muted-foreground">Created {new Date(webhook.created_at).toLocaleDateString()}</p>
        </div>
        {confirming ? (
          <div className="flex items-center gap-2">
            <Button variant="destructive" size="sm" loading={deleting} onClick={onDelete}>
              Delete
            </Button>
            <Button variant="ghost" size="sm" onClick={onCancel}>
              Cancel
            </Button>
          </div>
        ) : (
          <Button variant="ghost" size="sm" className="text-red-500 hover:text-red-600 hover:bg-red-50" aria-label="Delete legacy webhook" onClick={onAsk}>
            <Trash2 className="w-4 h-4" />
          </Button>
        )}
      </div>
    </div>
  );
}
