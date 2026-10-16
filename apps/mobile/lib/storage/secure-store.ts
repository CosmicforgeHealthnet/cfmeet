import * as SecureStore from "expo-secure-store";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { STORAGE_KEYS } from "@/lib/constants";

export interface StoredUserInfo {
  id: string;
  email: string;
  username: string;
  role: string;
  status: string;
}

export const tokenStorage = {
  async getAccessToken(): Promise<string | null> {
    try {
      return await SecureStore.getItemAsync(STORAGE_KEYS.ACCESS_TOKEN);
    } catch {
      return null;
    }
  },

  async setAccessToken(token: string): Promise<void> {
    try {
      await SecureStore.setItemAsync(STORAGE_KEYS.ACCESS_TOKEN, token);
    } catch {
      console.error("Failed to save access token");
    }
  },

  async getRefreshToken(): Promise<string | null> {
    try {
      return await SecureStore.getItemAsync(STORAGE_KEYS.REFRESH_TOKEN);
    } catch {
      return null;
    }
  },

  async setRefreshToken(token: string): Promise<void> {
    try {
      await SecureStore.setItemAsync(STORAGE_KEYS.REFRESH_TOKEN, token);
    } catch {
      console.error("Failed to save refresh token");
    }
  },

  async clearTokens(): Promise<void> {
    try {
      await SecureStore.deleteItemAsync(STORAGE_KEYS.ACCESS_TOKEN);
      await SecureStore.deleteItemAsync(STORAGE_KEYS.REFRESH_TOKEN);
    } catch {
      console.error("Failed to clear tokens");
    }
  },

  async setTokens(accessToken: string, refreshToken: string): Promise<void> {
    await this.setAccessToken(accessToken);
    await this.setRefreshToken(refreshToken);
  },
};

export const userStorage = {
  async getUserInfo(): Promise<StoredUserInfo | null> {
    try {
      const data = await AsyncStorage.getItem(STORAGE_KEYS.USER_INFO);
      if (data) {
        return JSON.parse(data) as StoredUserInfo;
      }
      return null;
    } catch {
      return null;
    }
  },

  async setUserInfo(user: StoredUserInfo): Promise<void> {
    try {
      await AsyncStorage.setItem(STORAGE_KEYS.USER_INFO, JSON.stringify(user));
    } catch {
      console.error("Failed to save user info");
    }
  },

  async clearUserInfo(): Promise<void> {
    try {
      await AsyncStorage.removeItem(STORAGE_KEYS.USER_INFO);
    } catch {
      console.error("Failed to clear user info");
    }
  },

  async getRememberMe(): Promise<boolean> {
    try {
      const value = await AsyncStorage.getItem(STORAGE_KEYS.REMEMBER_ME);
      return value === "true";
    } catch {
      return false;
    }
  },

  async setRememberMe(value: boolean): Promise<void> {
    try {
      await AsyncStorage.setItem(STORAGE_KEYS.REMEMBER_ME, String(value));
    } catch {
      console.error("Failed to save remember me preference");
    }
  },
};

export const clearAllStorage = async (): Promise<void> => {
  await tokenStorage.clearTokens();
  await userStorage.clearUserInfo();
};
