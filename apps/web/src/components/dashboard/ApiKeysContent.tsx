"use client";

import { useState } from "react";
import { Copy, Trash2, Plus, AlertCircle, Check, Key } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useApiKeys, useCreateApiKey, useRevokeApiKey } from "@/hooks";
import { CreateApiKeyResponse } from "@/types/api-keys";
import { cn } from "@/lib/utils";

export default function ApiKeysContent() {
  const { data: keys = [], isLoading, error: fetchError } = useApiKeys();
  const createApiKey = useCreateApiKey();
  const revokeApiKey = useRevokeApiKey();

  const [newKey, setNewKey] = useState<CreateApiKeyResponse | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");

  const handleCreate = () => {
    setError("");
    createApiKey.mutate(undefined, {
      onSuccess: (data) => {
        setNewKey(data);
      },
      onError: () => {
        setError("Failed to create API key");
      },
    });
  };

  const handleRevoke = (id: string) => {
    if (
      !confirm(
        "Are you sure you want to revoke this API key? This action cannot be undone.",
      )
    ) {
      return;
    }

    revokeApiKey.mutate(id, {
      onError: () => {
        setError("Failed to revoke API key");
      },
    });
  };

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  if (isLoading) {
    return (
      <div className="flex h-40 items-center justify-center">
        <Spinner />
      </div>
    );
  }

  // Combine errors
  const displayError = error || (fetchError ? "Failed to fetch API keys" : "");

  return (
    <div className="space-y-6">
      <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold mb-1">API Keys</h2>
          <p className="text-sm text-gray-500">
            Manage your API keys for third-party integrations.
          </p>
        </div>
        <Button onClick={handleCreate} loading={createApiKey.isPending}>
          <Plus className="w-4 h-4 mr-2" />
          Create New Key
        </Button>
      </div>

      {displayError && (
        <div className="bg-red-50 text-red-600 px-4 py-3 rounded-lg text-sm border border-red-100 flex items-center gap-2">
          <AlertCircle className="w-4 h-4" />
          {displayError}
        </div>
      )}

      {/* New Key Modal/Alert */}
      {newKey && (
        <div className="bg-green-50 border border-green-200 rounded-xl p-6 relative animate-in fade-in slide-in-from-top-4">
          <div className="flex items-start gap-3">
            <div className="p-2 bg-green-100 rounded-lg text-green-600">
              <Key className="w-5 h-5" />
            </div>
            <div className="space-y-3 flex-1">
              <h3 className="font-semibold text-green-900">
                API Key Created Successfully
              </h3>
              <p className="text-sm text-green-800">
                Please copy your API key now. For security reasons, it will
                never be shown again.
              </p>

              <div className="flex items-center gap-2">
                <code className="flex-1 bg-white border border-green-200 px-3 py-2 rounded-lg text-sm font-mono text-gray-800 break-all">
                  {newKey.api_key}
                </code>
                <Button
                  size="sm"
                  variant="outline"
                  className="bg-white border-green-200 text-green-700 hover:bg-green-50"
                  onClick={() => copyToClipboard(newKey.api_key)}
                >
                  {copied ? (
                    <Check className="w-4 h-4" />
                  ) : (
                    <Copy className="w-4 h-4" />
                  )}
                </Button>
              </div>

              <div className="flex justify-end">
                <Button
                  size="sm"
                  variant="ghost"
                  className="text-green-700 hover:text-green-800 hover:bg-green-100"
                  onClick={() => setNewKey(null)}
                >
                  Close
                </Button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Keys List */}
      <div className="space-y-4">
        {keys.length === 0 ? (
          <div className="text-center py-12 bg-gray-50 rounded-xl border border-dashed border-gray-200">
            <Key className="w-8 h-8 mx-auto text-gray-300 mb-3" />
            <p className="text-gray-500 font-medium">No API keys found</p>
            <p className="text-gray-400 text-sm">Create one to get started</p>
          </div>
        ) : (
          keys.map((key) => (
            <div
              key={key.id}
              className={cn(
                "bg-white rounded-xl border p-4 transition-all hover:shadow-sm",
                key.status === "revoked"
                  ? "border-gray-100 opacity-60 bg-gray-50"
                  : "border-gray-200",
              )}
            >
              <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4">
                <div className="space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-sm font-medium text-gray-900">
                      {key.api_key_masked}
                    </span>
                    <span
                      className={cn(
                        "px-2 py-0.5 rounded-full text-xs font-medium capitalize",
                        key.status === "active"
                          ? "bg-green-100 text-green-700"
                          : "bg-gray-100 text-gray-600",
                      )}
                    >
                      {key.status}
                    </span>
                  </div>
                  <div className="flex items-center gap-4 text-xs text-gray-500">
                    <span>
                      Created{" "}
                      {new Date(key.created_at).toLocaleDateString("en-US", {
                        year: "numeric",
                        month: "short",
                        day: "numeric",
                      })}
                    </span>
                    <span>•</span>
                    <span>
                      Expires{" "}
                      {new Date(key.expires_at).toLocaleDateString("en-US", {
                        year: "numeric",
                        month: "short",
                        day: "numeric",
                      })}
                    </span>
                  </div>
                </div>

                <div className="flex items-center gap-6">
                  <div className="text-right hidden sm:block">
                    <p className="text-xs text-gray-500 mb-0.5">Usage</p>
                    <p className="text-sm font-medium text-gray-900">
                      {key.used_count} / {key.usage_limit}
                    </p>
                  </div>

                  {key.status === "active" && (
                    <Button
                      variant="ghost"
                      size="sm"
                      className="text-red-600 hover:text-red-700 hover:bg-red-50"
                      loading={
                        revokeApiKey.isPending &&
                        revokeApiKey.variables === key.id
                      }
                      onClick={() => handleRevoke(key.id)}
                    >
                      <Trash2 className="w-4 h-4 mr-2" />
                      Revoke
                    </Button>
                  )}
                </div>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
