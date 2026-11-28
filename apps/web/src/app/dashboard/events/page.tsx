import { Suspense } from "react";
import EventsContent from "@/components/events/EventsContent";

export default function EventsPage() {
  return (
    <Suspense fallback={<p role="status" className="p-5 text-sm text-muted-foreground">Loading events…</p>}>
      <EventsContent />
    </Suspense>
  );
}
