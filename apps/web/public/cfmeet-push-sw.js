/* CF Meet notifications (Web Push). Payloads are metadata only: never message text. */
self.addEventListener("push", (event) => {
  let data = {};
  try { data = event.data ? event.data.json() : {}; } catch { data = {}; }
  const conversationId = typeof data.conversationId === "string" ? data.conversationId : null;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      // Skip the alert when a CF Meet tab is already open and in front.
      if (windows.some((w) => w.focused)) return undefined;
      return self.registration.showNotification("New message", {
        body: "Open CF Meet to read it.",
        icon: "/CFLogo.png",
        badge: "/CFLogo.png",
        tag: conversationId ? `chat-${conversationId}` : "chat",
        renotify: true,
        data: { url: conversationId ? `/dashboard/messages?conversation=${encodeURIComponent(conversationId)}` : "/dashboard/messages" },
      });
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = (event.notification.data && event.notification.data.url) || "/dashboard/messages";
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      const open = windows.find((w) => new URL(w.url).origin === self.location.origin);
      if (open) { open.focus(); return open.navigate(url); }
      return self.clients.openWindow(url);
    }),
  );
});
