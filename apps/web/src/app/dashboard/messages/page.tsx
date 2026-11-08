import { Suspense } from "react";
import MessagesContent from "@/components/messaging/MessagesContent";

export default function MessagesPage() {
  return (
    <Suspense fallback={<p role="status" className="p-5 text-sm text-muted-foreground">Loading messages…</p>}>
      <MessagesContent />
    </Suspense>
  );
}
