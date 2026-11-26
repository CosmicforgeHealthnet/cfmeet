"use client";

import { createContext, useContext, useEffect, useState } from "react";
import { MessagingEngine } from "@/lib/messaging/engine";

const EngineContext = createContext<MessagingEngine | null>(null);

/**
 * One messaging engine for the whole signed-in app (WhatsApp Web keeps its connection open
 * on every screen): the live connection receives messages and confirms their delivery
 * wherever the user is in CF Meet, so senders see ✓✓ as soon as a message reaches this
 * browser, not only once the chat is opened. Reading still happens only when the chat is
 * shown.
 */
export function MessagingEngineProvider({ children }: { children: React.ReactNode }) {
  const [engine] = useState(() => new MessagingEngine());
  useEffect(() => {
    void engine.start();
    return () => engine.stop();
  }, [engine]);
  return <EngineContext.Provider value={engine}>{children}</EngineContext.Provider>;
}

/** The app-wide engine, or null outside the signed-in app. */
export function useSharedMessagingEngine(): MessagingEngine | null {
  return useContext(EngineContext);
}
