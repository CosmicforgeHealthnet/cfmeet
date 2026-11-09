"use client";

import { useState } from "react";
import { Trash2, AlertCircle, Key } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useApiKeys, useRevokeApiKey } from "@/hooks";
import { cn } from "@/lib/utils";
import LegacyDeprecationNotice from "./LegacyDeprecationNotice";

const day = (value: string) =>
  new Date(value).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric" });

/**
 * Legacy API keys (deprecated, PLT-004): read-only list plus revoke. Creating keys moved to
 * the Developer Portal (`/developers/credentials`).
 */
export default function ApiKeysContent() {
  const { data: keys = [], isLoading, error: fetchError } = useApiKeys();
  const revokeApiKey = useRevokeApiKey();
  const [error, setError] = useState("");

  const handleRevoke = (id: string) => {
    if (!confirm("Revoke this legacy API key? Requests using it will fail. This cannot be undone.")) return;
    revokeApiKey.mutate(id, { onError: () => setError("Failed to revoke API key") });
  };

  if (isLoading) {
    return (
      <div className="flex h-40 items-center justify-center">
        <Spinner />
      </div>
    );
  }

  const displayError = error || (fetchError ? "Failed to fetch API keys" : "");

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-lg font-semibold mb-1">Legacy API keys</h2>
        <p className="text-sm text-muted-foreground">Keys issued by the old meeting API. Read-only: revoke them after migrating.</p>
      </div>

      <LegacyDeprecationNotice kind="api-keys" />

      {displayError && (
        <div className="bg-red-50 text-red-600 px-4 py-3 rounded-lg text-sm border border-red-100 flex items-center gap-2">
          <AlertCircle className="w-4 h-4" />
          {displayError}
        </div>
      )}

      <div className="space-y-4">
        {keys.length === 0 ? (
          <div className="text-center py-12 bg-muted rounded-xl border border-dashed border-border">
            <Key className="w-8 h-8 mx-auto text-muted-foreground mb-3" />
            <p className="text-muted-foreground font-medium">No legacy API keys</p>
            <p className="text-muted-foreground text-sm">Create API keys in the developer console.</p>
          </div>
        ) : (
          keys.map((key) => (
            <div
              key={key.id}
              className={cn("bg-card rounded-xl border border-border p-4", key.status === "revoked" && "opacity-60 bg-muted")}
            >
              <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4">
                <div className="space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-sm font-medium text-foreground">{key.api_key_masked}</span>
                    <span
                      className={cn(
                        "px-2 py-0.5 rounded-full text-xs font-medium capitalize",
                        key.status === "active" ? "bg-green-100 text-green-700" : "bg-muted text-muted-foreground",
                      )}
                    >
                      {key.status}
                    </span>
                    <span className="px-2 py-0.5 rounded-full text-xs font-medium bg-amber-100 text-amber-800">deprecated</span>
                  </div>
                  <div className="flex items-center gap-4 text-xs text-muted-foreground">
                    <span>Created {day(key.created_at)}</span>
                    <span>•</span>
                    <span>Expires {day(key.expires_at)}</span>
                  </div>
                </div>

                <div className="flex items-center gap-6">
                  <div className="text-right hidden sm:block">
                    <p className="text-xs text-muted-foreground mb-0.5">Usage</p>
                    <p className="text-sm font-medium text-foreground">
                      {key.used_count} / {key.usage_limit}
                    </p>
                  </div>
                  {key.status === "active" && (
                    <Button
                      variant="ghost"
                      size="sm"
                      className="text-red-600 hover:text-red-700 hover:bg-red-50"
                      loading={revokeApiKey.isPending && revokeApiKey.variables === key.id}
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
