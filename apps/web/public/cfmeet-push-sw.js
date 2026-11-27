/* CF Meet notifications (Web Push). Payloads are metadata only: never message text.
 * Sent by Messaging today and, after the push cutover (Notification Service N9), by the
 * Notification Service with the same VAPID key and the same metadata keys (values may
 * arrive as strings): type "message" (conversationId), "call" (an incoming call; the
 * call itself is recovered through Calling), "inbox" (a new in-app notification). */
const ROUTES = {
  message: { title: "New message", body: "Open CF Meet to read it.", tag: "chat", url: "/dashboard/messages" },
  call: { title: "Incoming call", body: "Open CF Meet to answer.", tag: "cfmeet-call", url: "/dashboard/calls" },
  inbox: { title: "CF Meet", body: "You have a new notification.", tag: "cfmeet-inbox", url: "/dashboard" },
};

self.addEventListener("push", (event) => {
  let data = {};
  try { data = event.data ? event.data.json() : {}; } catch { data = {}; }
  const kind = Object.prototype.hasOwnProperty.call(ROUTES, data.type) ? data.type : "message";
  const route = ROUTES[kind];
  const conversationId = kind === "message" && typeof data.conversationId === "string" ? data.conversationId : null;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      // Skip the alert when a CF Meet tab is already open and in front.
      if (windows.some((w) => w.focused)) return undefined;
      return self.registration.showNotification(route.title, {
        body: route.body,
        icon: "/CFLogo.png",
        badge: "/CFLogo.png",
        tag: conversationId ? `chat-${conversationId}` : route.tag,
        renotify: true,
        requireInteraction: kind === "call",
        data: { url: conversationId ? `/dashboard/messages?conversation=${encodeURIComponent(conversationId)}` : route.url },
      });
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const raw = (event.notification.data && event.notification.data.url) || "/dashboard/messages";
  // Same-origin paths only.
  const url = typeof raw === "string" && raw.startsWith("/") && !raw.startsWith("//") ? raw : "/dashboard";
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      // Never navigate a tab that is in a meeting or a call (that would end it): reuse an
      // app tab that is not (the focused one first), otherwise open a new tab.
      const ours = windows.filter((w) => new URL(w.url).origin === self.location.origin);
      const free = (w) => /^\/(dashboard|settings|organizations|developers)(\/|$)/.test(new URL(w.url).pathname);
      const open = ours.find((w) => w.focused && free(w)) || ours.find(free);
      if (open) return open.focus().then(() => open.navigate(url));
      return self.clients.openWindow(url);
    }),
  );
});
