/**
 * Timezone utilities for CosmicForge RTC.
 *
 * All times from the backend are in UTC (ending with "Z").
 * These utilities help with:
 * - Detecting the user's IANA timezone
 * - Formatting UTC strings for display in the user's local timezone
 */

/**
 * Get the user's IANA timezone string (e.g., "Africa/Lagos", "America/New_York").
 */
export function getUserTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/**
 * Ensure a datetime string is treated as UTC.
 * If the string doesn't end with "Z" or contain a timezone offset, append "Z".
 */
function ensureUtc(dateStr: string): string {
  if (dateStr.endsWith("Z") || /[+-]\d{2}:\d{2}$/.test(dateStr)) {
    return dateStr;
  }
  return `${dateStr}Z`;
}

/**
 * Format a UTC datetime string as a local date string.
 *
 * Example output: "January 25, 2026"
 */
export function formatDateForDisplay(utcString: string): string {
  const date = new Date(ensureUtc(utcString));
  return date.toLocaleDateString("en-US", {
    day: "numeric",
    month: "long",
    year: "numeric",
  });
}

/**
 * Format a UTC datetime string as a short local date string.
 *
 * Example output: "Wed, Jan 25, 2026"
 */
export function formatShortDateForDisplay(utcString: string): string {
  const date = new Date(ensureUtc(utcString));
  return date.toLocaleDateString("en-US", {
    weekday: "short",
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

/**
 * Format a UTC datetime string as a local time string.
 *
 * Example output: "2:00 PM"
 */
export function formatTimeForDisplay(utcString: string): string {
  const date = new Date(ensureUtc(utcString));
  return date.toLocaleTimeString("en-US", {
    hour: "numeric",
    minute: "2-digit",
    hour12: true,
  });
}

/**
 * Extract a local date value (YYYY-MM-DD) from a UTC datetime string.
 * Useful for pre-populating date input fields.
 */
export function toLocalDateValue(utcString: string): string {
  const date = new Date(ensureUtc(utcString));
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/**
 * Extract a local time value (HH:mm) from a UTC datetime string.
 * Useful for pre-populating time input fields.
 */
export function toLocalTimeValue(utcString: string): string {
  const date = new Date(ensureUtc(utcString));
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  return `${hours}:${minutes}`;
}
