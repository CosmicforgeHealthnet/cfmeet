/** Native-token registration lifecycle, prepared for a signed mobile build.
 * The caller supplies a server-approved Account Device and the SDK 57 native token
 * source. This does not request an Expo push-service token or implement call UI. */
export type NativePushToken = { platform: "APNS" | "FCM"; handle: string };
export interface NativePushSource {
  permission(): Promise<boolean>;
  token(): Promise<NativePushToken>;
  subscribe(listener: (token: NativePushToken) => void): () => void;
}
export interface NativePushApi {
  register(device: string, token: NativePushToken): Promise<unknown>;
  remove(device: string, platform: NativePushToken["platform"]): Promise<unknown>;
}

/** Await stop BEFORE clearing the bearer session on logout/device replacement.
 * It drains in-flight writes before removal, so token refresh cannot resurrect a
 * registration after logout. The server still validates device ownership/state. */
export function nativePushRegistration(api: NativePushApi) {
  let active = false;
  let stopSource: (() => void) | null = null;
  let device: string | null = null;
  const platforms = new Set<NativePushToken["platform"]>();
  let writes: Promise<void> = Promise.resolve();
  let onError: (() => void) | null = null;
  let generation = 0;
  const update = (token: NativePushToken) => {
    if (!active || !device || !token.handle || token.handle.length > 4096) return;
    const target = device;
    const epoch = generation;
    writes = writes.catch(() => {}).then(async () => {
      if (!active || device !== target || generation !== epoch) return;
      // Include attempts whose HTTP response is lost: removal is idempotent.
      platforms.add(token.platform);
      await api.register(target, token);
    });
    void writes.catch(() => onError?.());
  };
  return {
    async start(accountDeviceId: string, source: NativePushSource, enabled: boolean, error?: () => void) {
      if (!enabled) return false;
      if (active || device) throw new Error("Stop the previous push registration first");
      if (!/^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i.test(accountDeviceId)) throw new Error("A declared Account Device is required");
      active = true; device = accountDeviceId; onError = error ?? null;
      const epoch = ++generation;
      try {
        const allowed = await source.permission();
        if (generation !== epoch || !active) return false;
        if (!allowed) { active = false; device = null; return false; }
        const token = await source.token();
        if (!active || generation !== epoch) return false;
        stopSource = source.subscribe((value) => { if (generation === epoch) update(value); });
        update(token);
        await writes;
        return active;
      } catch {
        if (generation !== epoch) return false;
        active = false; stopSource?.(); stopSource = null;
        // Keep attempted platforms/device for stop() even after an ambiguous write.
        if (!platforms.size) device = null;
        onError?.();
        return false;
      }
    },
    async stop() {
      generation++;
      active = false; stopSource?.(); stopSource = null;
      const target = device;
      await writes.catch(() => {});
      if (target) {
        for (const platform of platforms) await api.remove(target, platform);
      }
      platforms.clear(); device = null; onError = null;
    },
  };
}
