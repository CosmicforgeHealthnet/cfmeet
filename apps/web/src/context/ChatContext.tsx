"use client";

import { createContext, useContext } from "react";
import type { ChatMessage } from "@/types/meeting";
import type { UseMutationResult } from "@tanstack/react-query";

interface SendChatResponse {
  success: boolean;
  message: string;
  data: ChatMessage;
}

interface ChatContextType {
  messages: ChatMessage[];
  isLoading: boolean;
  sendMessage: UseMutationResult<SendChatResponse, Error, string, unknown>;
}

export const ChatContext = createContext<ChatContextType | null>(null);

export function useChatContext() {
  const context = useContext(ChatContext);
  if (!context) {
    throw new Error("useChatContext must be used within a ChatProvider");
  }
  return context;
}
