// How long ago something happened, as compact lists show it: `now`, `5m`, `3h`, `2d`, `4w`, then
// the date.

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;

/** Times before this are not timestamps (lists use small numbers as ordering keys). */
const EARLIEST = Date.UTC(2020, 0, 1);

/** `at` (Unix milliseconds) relative to `now`; empty when `at` is not a time. */
export function formatAgo(at: number | null | undefined, now = Date.now()): string {
  if (!at || at < EARLIEST) return "";
  const since = Math.max(0, now - at);
  if (since < MINUTE) return "now";
  if (since < HOUR) return `${Math.floor(since / MINUTE)}m`;
  if (since < DAY) return `${Math.floor(since / HOUR)}h`;
  if (since < WEEK) return `${Math.floor(since / DAY)}d`;
  if (since < 5 * WEEK) return `${Math.floor(since / WEEK)}w`;
  const date = new Date(at);
  const sameYear = date.getFullYear() === new Date(now).getFullYear();
  return date.toLocaleDateString([], sameYear ? { month: "short", day: "numeric" } : { month: "short", year: "numeric" });
}
