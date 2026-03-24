"use client";

import { useState } from "react";
import { useWebhooks, useCreateWebhook, useUpdateWebhook, useDeleteWebhook } from "@/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Plus, Trash2, Copy, Check, Pencil } from "lucide-react";
import type { Webhook, CreateWebhookResponse } from "@/types/webhook";
import { WEBHOOK_EVENT_TYPES } from "@/types/webhook";

export default function WebhooksContent() {
  const { data: webhooks, isLoading, isError } = useWebhooks();
  const createWebhook = useCreateWebhook();
  const updateWebhook = useUpdateWebhook();
  const deleteWebhook = useDeleteWebhook();

  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [endpointUrl, setEndpointUrl] = useState("");
  const [eventType, setEventType] = useState("");
  const [createError, setCreateError] = useState("");

  const [deleteId, setDeleteId] = useState<string | null>(null);
  const [createdWebhook, setCreatedWebhook] = useState<CreateWebhookResponse | null>(null);

  // Edit state
  const [editingWebhook, setEditingWebhook] = useState<Webhook | null>(null);
  const [editEndpointUrl, setEditEndpointUrl] = useState("");
  const [editStatus, setEditStatus] = useState("");
  const [editError, setEditError] = useState("");

  const handleCreate = () => {
    setCreateError("");

    if (!endpointUrl.trim()) {
      setCreateError("Endpoint URL is required");
      return;
    }

    if (!eventType) {
      setCreateError("Event type is required");
      return;
    }

    try {
      new URL(endpointUrl);
    } catch {
      setCreateError("Please enter a valid URL");
      return;
    }

    createWebhook.mutate(
      { endpoint_url: endpointUrl.trim(), event_type: eventType },
      {
        onSuccess: (data) => {
          setShowCreateDialog(false);
          setEndpointUrl("");
          setEventType("");
          setCreatedWebhook(data);
        },
        onError: () => {
          setCreateError("Failed to create webhook");
        },
      }
    );
  };

  const handleEdit = (webhook: Webhook) => {
    setEditingWebhook(webhook);
    setEditEndpointUrl(webhook.endpoint_url);
    setEditStatus(webhook.status);
    setEditError("");
  };

  const handleUpdate = () => {
    if (!editingWebhook) return;
    setEditError("");

    if (!editEndpointUrl.trim()) {
      setEditError("Endpoint URL is required");
      return;
    }

    try {
      new URL(editEndpointUrl);
    } catch {
      setEditError("Please enter a valid URL");
      return;
    }

    updateWebhook.mutate(
      {
        id: editingWebhook.id,
        data: {
          endpoint_url: editEndpointUrl.trim(),
          status: editStatus,
        },
      },
      {
        onSuccess: () => {
          setEditingWebhook(null);
        },
        onError: () => {
          setEditError("Failed to update webhook");
        },
      }
    );
  };

  const handleDelete = (id: string) => {
    deleteWebhook.mutate(id, {
      onSuccess: () => setDeleteId(null),
    });
  };

  if (isLoading) {
    return (
      <div className="flex h-40 items-center justify-center">
        <Spinner />
      </div>
    );
  }

  if (isError) {
    return (
      <div className="text-center py-10">
        <p className="text-red-500">Failed to load webhooks</p>
      </div>
    );
  }

  return (
    <div className="space-y-6 animate-in fade-in slide-in-from-bottom-4 duration-500">
      <div className="bg-white p-6 rounded-2xl shadow-sm border border-gray-100 space-y-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold mb-1">Webhooks</h2>
            <p className="text-sm text-gray-500">
              Receive real-time notifications for meeting events.
            </p>
          </div>
          <Dialog open={showCreateDialog} onOpenChange={setShowCreateDialog}>
            <DialogTrigger asChild>
              <Button className="gap-2">
                <Plus className="w-4 h-4" />
                Add Webhook
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Create Webhook</DialogTitle>
                <DialogDescription>
                  Add a new webhook endpoint to receive event notifications.
                </DialogDescription>
              </DialogHeader>

              <div className="space-y-4 py-4">
                <div className="space-y-2">
                  <Label htmlFor="endpoint-url">Endpoint URL</Label>
                  <Input
                    id="endpoint-url"
                    placeholder="https://your-server.com/webhook"
                    value={endpointUrl}
                    onChange={(e) => setEndpointUrl(e.target.value)}
                  />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="event-type">Event Type</Label>
                  <select
                    id="event-type"
                    value={eventType}
                    onChange={(e) => setEventType(e.target.value)}
                    className="flex h-9 sm:h-10 w-full rounded-xl border border-[#029CD44D] bg-transparent px-3 sm:px-5 py-2 text-sm sm:text-base font-medium focus:outline-none focus:border-[#029CD4] focus:ring-1 focus:ring-[#029CD4]"
                  >
                    <option value="">Select an event type</option>
                    {WEBHOOK_EVENT_TYPES.map((type) => (
                      <option key={type.value} value={type.value}>
                        {type.label}
                      </option>
                    ))}
                  </select>
                </div>

                {createError && (
                  <p className="text-sm text-red-500">{createError}</p>
                )}
              </div>

              <DialogFooter>
                <Button
                  variant="outline"
                  onClick={() => setShowCreateDialog(false)}
                >
                  Cancel
                </Button>
                <Button
                  onClick={handleCreate}
                  loading={createWebhook.isPending}
                >
                  Create Webhook
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </div>

        {webhooks && webhooks.length === 0 ? (
          <div className="text-center py-10 text-gray-500">
            <p>No webhooks configured yet.</p>
            <p className="text-sm mt-1">
              Create a webhook to start receiving event notifications.
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {webhooks?.map((webhook) => (
              <WebhookCard
                key={webhook.id}
                webhook={webhook}
                onEdit={() => handleEdit(webhook)}
                onDelete={() => setDeleteId(webhook.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* Edit Webhook Dialog */}
      <Dialog open={!!editingWebhook} onOpenChange={() => setEditingWebhook(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit Webhook</DialogTitle>
            <DialogDescription>
              Update the webhook endpoint URL or status.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="edit-endpoint-url">Endpoint URL</Label>
              <Input
                id="edit-endpoint-url"
                placeholder="https://your-server.com/webhook"
                value={editEndpointUrl}
                onChange={(e) => setEditEndpointUrl(e.target.value)}
              />
            </div>

            <div className="space-y-2">
              <Label htmlFor="edit-status">Status</Label>
              <select
                id="edit-status"
                value={editStatus}
                onChange={(e) => setEditStatus(e.target.value)}
                className="flex h-9 sm:h-10 w-full rounded-xl border border-[#029CD44D] bg-transparent px-3 sm:px-5 py-2 text-sm sm:text-base font-medium focus:outline-none focus:border-[#029CD4] focus:ring-1 focus:ring-[#029CD4]"
              >
                <option value="active">Active</option>
                <option value="inactive">Inactive</option>
              </select>
            </div>

            {editingWebhook && (
              <div className="space-y-2">
                <Label>Event Type</Label>
                <p className="text-sm text-gray-500">
                  {WEBHOOK_EVENT_TYPES.find((t) => t.value === editingWebhook.event_type)?.label || editingWebhook.event_type}
                </p>
                <p className="text-xs text-gray-400">Event type cannot be changed.</p>
              </div>
            )}

            {editError && (
              <p className="text-sm text-red-500">{editError}</p>
            )}
          </div>

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setEditingWebhook(null)}
            >
              Cancel
            </Button>
            <Button
              onClick={handleUpdate}
              loading={updateWebhook.isPending}
            >
              Save Changes
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Confirmation Dialog */}
      <Dialog open={!!deleteId} onOpenChange={() => setDeleteId(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Webhook</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete this webhook? This action cannot
              be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteId(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={() => deleteId && handleDelete(deleteId)}
              loading={deleteWebhook.isPending}
            >
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Created Webhook Secret Dialog */}
      <Dialog open={!!createdWebhook} onOpenChange={() => setCreatedWebhook(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Webhook Created Successfully</DialogTitle>
            <DialogDescription>
              Save your webhook secret now. It will not be shown again.
            </DialogDescription>
          </DialogHeader>
          {createdWebhook && (
            <div className="space-y-4 py-4">
              <div className="space-y-2">
                <Label>Endpoint URL</Label>
                <p className="text-sm text-gray-700 break-all">
                  {createdWebhook.endpoint_url}
                </p>
              </div>
              <div className="space-y-2">
                <Label>Event Type</Label>
                <p className="text-sm text-gray-700">
                  {WEBHOOK_EVENT_TYPES.find((t) => t.value === createdWebhook.event_type)?.label || createdWebhook.event_type}
                </p>
              </div>
              <div className="space-y-2">
                <Label>Secret Key</Label>
                <div className="flex items-center gap-2">
                  <code className="flex-1 text-sm bg-gray-100 px-3 py-2 rounded font-mono break-all">
                    {createdWebhook.secret}
                  </code>
                  <CopySecretButton secret={createdWebhook.secret} />
                </div>
                <p className="text-xs text-amber-600">
                  Copy and save this secret securely. You won&apos;t be able to see it again.
                </p>
              </div>
            </div>
          )}
          <DialogFooter>
            <Button onClick={() => setCreatedWebhook(null)}>
              Done
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function CopySecretButton({ secret }: { secret: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    navigator.clipboard.writeText(secret);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <Button variant="outline" size="sm" onClick={handleCopy}>
      {copied ? (
        <Check className="w-4 h-4 text-green-500" />
      ) : (
        <Copy className="w-4 h-4" />
      )}
    </Button>
  );
}

function WebhookCard({
  webhook,
  onEdit,
  onDelete,
}: {
  webhook: Webhook;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const eventLabel =
    WEBHOOK_EVENT_TYPES.find((t) => t.value === webhook.event_type)?.label ||
    webhook.event_type;

  return (
    <div className="p-4 border border-gray-100 rounded-xl bg-gray-50/50">
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0 space-y-2">
          <div className="flex items-center gap-2">
            <span
              className={`px-2 py-0.5 rounded-full text-xs font-medium ${
                webhook.status === "active"
                  ? "bg-green-100 text-green-700"
                  : "bg-gray-100 text-gray-600"
              }`}
            >
              {webhook.status}
            </span>
            <span className="px-2 py-0.5 rounded-full text-xs font-medium bg-blue-100 text-blue-700">
              {eventLabel}
            </span>
          </div>

          <p className="text-sm font-medium text-gray-900 truncate">
            {webhook.endpoint_url}
          </p>

          <p className="text-xs text-gray-400">
            Created {new Date(webhook.created_at).toLocaleDateString()}
          </p>
        </div>

        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="sm"
            className="text-gray-500 hover:text-[#029CD4] hover:bg-blue-50"
            onClick={onEdit}
          >
            <Pencil className="w-4 h-4" />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="text-red-500 hover:text-red-600 hover:bg-red-50"
            onClick={onDelete}
          >
            <Trash2 className="w-4 h-4" />
          </Button>
        </div>
      </div>
    </div>
  );
}
