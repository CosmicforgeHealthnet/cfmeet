import Cookies from "js-cookie";

const ACCESS_TOKEN_KEY = "access_token";
const REFRESH_TOKEN_KEY = "refresh_token";
const REMEMBER_ME_KEY = "remember_me";

export const cookieStore = {
  setTokens(accessToken: string, refreshToken: string, rememberMe?: boolean) {
    // If rememberMe is passed, persist the preference
    if (rememberMe !== undefined) {
      this.setRememberMe(rememberMe);
    }

    const persist = this.getRememberMe();
// Set cookies with appropriate expiration based on rememberMe
    if (persist) {
      // Persistent: access token 15 min, refresh token 7 days
      Cookies.set(ACCESS_TOKEN_KEY, accessToken, {
        expires: 1 / 96,
        secure: true,
        sameSite: "lax",
      });
      Cookies.set(REFRESH_TOKEN_KEY, refreshToken, {
        expires: 7,
        secure: true,
        sameSite: "lax",
      });
    } else {
      // Session only: no expires = cleared when browser closes
      Cookies.set(ACCESS_TOKEN_KEY, accessToken, {
        secure: true,
        sameSite: "lax",
      });
      Cookies.set(REFRESH_TOKEN_KEY, refreshToken, {
        secure: true,
        sameSite: "lax",
      });
    }
  },

  getAccessToken() {
    return Cookies.get(ACCESS_TOKEN_KEY);
  },

  getRefreshToken() {
    return Cookies.get(REFRESH_TOKEN_KEY);
  },

  clearTokens() {
    Cookies.remove(ACCESS_TOKEN_KEY);
    Cookies.remove(REFRESH_TOKEN_KEY);
    Cookies.remove(REMEMBER_ME_KEY);
  },

  setRememberMe(value: boolean) {
    if (typeof window !== "undefined") {
      localStorage.setItem(REMEMBER_ME_KEY, JSON.stringify(value));
    }
  },

  getRememberMe(): boolean {
    if (typeof window !== "undefined") {
      return localStorage.getItem(REMEMBER_ME_KEY) === "true";
    }
    return false;
  },
};
