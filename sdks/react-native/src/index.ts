// CF Meet platform SDK for React Native.
//
// Everything of the TypeScript SDK (`@cfmeet/platform-sdk`: CfMeetDeviceClient, chat,
// calls, meetings, push, user tokens, errors, realtime) is re-exported unchanged, plus:
//   * `createDeviceClient` / `createInstallationIdStore`: the installation id persisted in
//     the app's AsyncStorage (no storage dependency);
//   * `NativeCallBridge` + `connectNativeCalls`: the typed contract of the app's native
//     module for push tokens (PushKit VoIP, APNs, FCM) and the system call UI (CallKit,
//     ConnectionService), and the glue that drives it from a call session;
//   * React hooks: `useCfMeetChat`, `useCfMeetCall`, `useCfMeetMeeting`.

import { CfMeetDeviceClient, type CfMeetDeviceClientConfig } from '@cfmeet/platform-sdk';

import { asyncStorageAdapter, type AsyncStorageLike } from './storage.js';

export * from '@cfmeet/platform-sdk';

export { asyncStorageAdapter, createInstallationIdStore } from './storage.js';
export type { AsyncStorageLike } from './storage.js';
export type {
  NativeCallBridge,
  NativeCallEndReason,
  NativeIncomingCall,
  NativePushPlatform,
  NativePushToken,
  Unsubscribe,
} from './native-bridge.js';
export { connectNativeCalls } from './native-calls.js';
export type { ConnectNativeCallsOptions } from './native-calls.js';
export { useCfMeetCall, useCfMeetChat, useCfMeetMeeting } from './hooks.js';
export type {
  CfMeetConnectionStatus,
  UseCfMeetCall,
  UseCfMeetCallOptions,
  UseCfMeetChat,
  UseCfMeetChatOptions,
  UseCfMeetMeeting,
} from './hooks.js';

export interface ReactNativeDeviceClientConfig extends Omit<CfMeetDeviceClientConfig, 'storage'> {
  /** The app's AsyncStorage (`@react-native-async-storage/async-storage`) or a compatible store. */
  readonly storage: AsyncStorageLike;
}

/** A device client whose installation id persists in AsyncStorage. */
export function createDeviceClient(config: ReactNativeDeviceClientConfig): CfMeetDeviceClient {
  return new CfMeetDeviceClient({ ...config, storage: asyncStorageAdapter(config.storage) });
}
