// The installation id on React Native: persisted through the app's AsyncStorage (or any
// storage with the same getItem / setItem shape: MMKV, expo-secure-store adapters, ...).
// The SDK has NO storage dependency: the app passes its own instance.
//
//   import AsyncStorage from '@react-native-async-storage/async-storage';
//   const device = createDeviceClient({ ..., storage: AsyncStorage });

import { InstallationIdStore, type KeyValueStorage } from '@cfmeet/platform-sdk';

/** The part of `@react-native-async-storage/async-storage` the SDK uses. */
export interface AsyncStorageLike {
  getItem(key: string): Promise<string | null>;
  setItem(key: string, value: string): Promise<void>;
}

/** Adapts an AsyncStorage-like object (already compatible; kept for clarity and typing). */
export function asyncStorageAdapter(storage: AsyncStorageLike): KeyValueStorage {
  return {
    getItem: (key) => storage.getItem(key),
    setItem: (key, value) => storage.setItem(key, value),
  };
}

/** An installation-id store persisted in AsyncStorage under `key` (default `cfmeet.installationId`). */
export function createInstallationIdStore(storage: AsyncStorageLike, key?: string): InstallationIdStore {
  return new InstallationIdStore({ storage: asyncStorageAdapter(storage), key });
}
