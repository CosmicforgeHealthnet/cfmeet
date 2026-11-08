import * as SecureStore from "expo-secure-store";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { STORAGE_KEYS } from "@/lib/constants";

/**
 * Account tokens live only in SecureStore (Keychain / Keystore) and are never logged.
 * Non-secret user info and preferences live in AsyncStorage.
 */
export interface StoredUserInfo {
  id: string;
  username: string;
  role: string;
  status: string;
  email?: string;
}

export const tokenStorage = {
  async getAccessToken(): Promise<string | null> {
    try {
      return await SecureStore.getItemAsync(STORAGE_KEYS.ACCESS_TOKEN);
    } catch {
      return null;
    }
  },

  async getRefreshToken(): Promise<string | null> {
    try {
      return await SecureStore.getItemAsync(STORAGE_KEYS.REFRESH_TOKEN);
    } catch {
      return null;
    }
  },

  async setTokens(accessToken: string, refreshToken: string): Promise<void> {
    await SecureStore.setItemAsync(STORAGE_KEYS.ACCESS_TOKEN, accessToken);
    await SecureStore.setItemAsync(STORAGE_KEYS.REFRESH_TOKEN, refreshToken);
  },

  async clearTokens(): Promise<void> {
    await Promise.allSettled([
      SecureStore.deleteItemAsync(STORAGE_KEYS.ACCESS_TOKEN),
      SecureStore.deleteItemAsync(STORAGE_KEYS.REFRESH_TOKEN),
    ]);
  },
};

export const userStorage = {
  async getUserInfo(): Promise<StoredUserInfo | null> {
    try {
      const data = await AsyncStorage.getItem(STORAGE_KEYS.USER_INFO);
      return data ? (JSON.parse(data) as StoredUserInfo) : null;
    } catch {
      return null;
    }
  },

  async setUserInfo(user: StoredUserInfo): Promise<void> {
    try {
      await AsyncStorage.setItem(STORAGE_KEYS.USER_INFO, JSON.stringify(user));
    } catch {
      // Non-critical: the session still works; the profile is re-read from the server.
    }
  },

  async clearUserInfo(): Promise<void> {
    try {
      await AsyncStorage.removeItem(STORAGE_KEYS.USER_INFO);
    } catch {
      // Nothing else to do.
    }
  },

  async getRememberMe(): Promise<boolean> {
    try {
      return (await AsyncStorage.getItem(STORAGE_KEYS.REMEMBER_ME)) === "true";
    } catch {
      return false;
    }
  },

  async setRememberMe(value: boolean): Promise<void> {
    try {
      await AsyncStorage.setItem(STORAGE_KEYS.REMEMBER_ME, String(value));
    } catch {
      // Preference only.
    }
  },
};

export const clearAllStorage = async (): Promise<void> => {
  await tokenStorage.clearTokens();
  await userStorage.clearUserInfo();
  try {
    await AsyncStorage.removeItem(STORAGE_KEYS.WORKSPACE);
  } catch {
    // Nothing else to do.
  }
};
