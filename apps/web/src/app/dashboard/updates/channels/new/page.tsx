import Link from "next/link";
import { ArrowLeft } from "lucide-react";
import ChannelForm from "@/components/channels/ChannelForm";

export default function NewChannelPage() {
  return (
    <div className="min-h-dvh">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border bg-background px-2 py-2">
        <Link href="/dashboard/updates/channels/discover" aria-label="Back" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        <h1 className="text-lg font-semibold">New channel</h1>
      </header>
      <ChannelForm />
    </div>
  );
}
