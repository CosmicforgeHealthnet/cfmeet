import Link from "next/link";
import { AlertTriangle, ArrowRight } from "lucide-react";

/**
 * Section 16 (PLT-004 / PLT-003, DEPRECATED_WITH_MIGRATION): legacy meeting-service API keys
 * (`/api/v1/api-keys`) and meeting webhooks (`/api/v1/webhooks`) are deprecated. These
 * screens are read-only (plus revoke / delete, the migration's clean-up step) and point to
 * the Developer Portal, where keys are scoped service-account credentials and webhooks are
 * signed Developer Platform subscriptions.
 */
export default function LegacyDeprecationNotice({ kind }: { kind: "api-keys" | "webhooks" }) {
  return (
    <div role="note" className="flex gap-3 rounded-xl border border-amber-500/40 bg-amber-500/5 p-4 text-sm">
      <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-600" />
      <div className="space-y-2">
        <p className="font-semibold text-foreground">
          {kind === "api-keys" ? "Legacy API keys are deprecated" : "Legacy meeting webhooks are deprecated and never delivered"}
        </p>
        <p className="text-muted-foreground">
          {kind === "api-keys"
            ? "New legacy keys can no longer be created here. Use the CF Meet developer console: create a developer workspace, a project and a service account, then issue a scoped API key. Revoke your legacy keys once your integration has moved."
            : "CF Meet stores these registrations but sends no events to them. Use Developer Platform webhooks in the developer console for signed delivery with history and replay, then delete the registrations below."}
        </p>
        <Link href="/developers" className="inline-flex items-center gap-1 font-medium text-primary underline">
          Open the developer console <ArrowRight className="h-4 w-4" />
        </Link>
      </div>
    </div>
  );
}
