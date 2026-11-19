"use client";

import { Suspense } from "react";
import { useSearchParams } from "next/navigation";
import CallsContent from "@/components/calling/CallsContent";

const ID = /^[0-9a-f-]{8,64}$/i;

function Calls() {
  const params = useSearchParams();
  const call = params.get("call") ?? "";
  // ?contactId= / ?conversationId= (+ kind) come from Call buttons elsewhere: start that call here.
  const contactId = params.get("contactId") ?? "";
  const conversationId = params.get("conversationId") ?? "";
  const kind = params.get("kind") === "VIDEO" ? "VIDEO" : "VOICE";
  const name = (params.get("name") ?? "").slice(0, 80) || null;
  // ?tab=voicemail / ?tab=missed (a voicemail or missed call in the bell) opens that list.
  const tabParam = params.get("tab");
  const tab = tabParam === "voicemail" || tabParam === "missed" ? tabParam : "all";
  const launch = ID.test(contactId) ? { contactId, kind, name } as const : ID.test(conversationId) ? { conversationId, kind, name } as const : null;
  return <CallsContent initialCallId={ID.test(call) ? call : ""} launch={launch} initialTab={tab} />;
}

export default function CallingPage() {
  return <Suspense><Calls /></Suspense>;
}
