// Text helpers for tool-call rows in the transcript (components/ToolCall.tsx).

/** Characters shown before a long argument or result folds behind "show more". */
export const CLIP_CHARS = 1_200;
/** Lines shown before a long argument or result folds behind "show more". */
export const CLIP_LINES = 16;

/** A tool's run time: `340 ms`, `2.4 s`, or `3m 05s`. */
export function formatToolDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)}m ${String(total % 60).padStart(2, "0")}s`;
}

/** JSON arguments indented for reading; text that does not parse (cut short, or not JSON) is
 * returned as is. An empty object reads as empty. */
export function prettyArguments(raw: string): string {
  const text = raw.trim();
  if (!text) return "";
  try {
    const value: unknown = JSON.parse(text);
    if (value && typeof value === "object" && Object.keys(value).length === 0) return "";
    return JSON.stringify(value, null, 2);
  } catch {
    return text;
  }
}

/** The part of `text` shown before "show more", or `null` when it is short enough to show whole. */
export function clipText(text: string, chars: number = CLIP_CHARS, lines: number = CLIP_LINES): string | null {
  let end = Math.min(text.length, chars);
  let seen = 0;
  for (let i = 0; i < end; i++) {
    if (text[i] === "\n" && ++seen === lines) {
      end = i;
      break;
    }
  }
  return end < text.length ? text.slice(0, end) : null;
}
