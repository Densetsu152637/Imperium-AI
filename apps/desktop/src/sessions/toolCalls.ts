// Helpers for tool-call rows in the transcript (components/ToolCall.tsx) and for the summary
// line that folds consecutive calls together (components/ToolGroup.tsx).

import type { ToolEntry } from "./sessions";

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

/** One transcript item: an entry shown as itself, or a run of two or more consecutive tool calls
 * shown as one summary line. `index` is where the item's (first) entry sits in the source list. */
export type ToolRunItem<E> =
  | { kind: "entry"; entry: E; index: number }
  | { kind: "tools"; calls: ToolEntry[]; index: number };

/** Folds each run of two or more consecutive tool calls (no other entry between them) into one
 * item; a lone call and every other entry stay as they are. */
export function groupToolRuns<E extends { kind: string }>(entries: readonly E[]): ToolRunItem<E>[] {
  const items: ToolRunItem<E>[] = [];
  let start = 0;
  let calls: E[] = [];
  const flush = () => {
    if (calls.length === 1) items.push({ kind: "entry", entry: calls[0], index: start });
    else if (calls.length > 1) items.push({ kind: "tools", calls: calls as unknown as ToolEntry[], index: start });
    calls = [];
  };
  entries.forEach((entry, index) => {
    if (entry.kind === "tool") {
      if (calls.length === 0) start = index;
      calls.push(entry);
    } else {
      flush();
      items.push({ kind: "entry", entry, index });
    }
  });
  flush();
  return items;
}

/** The position of the item that shows source entry `entryIndex`, or `null` when none does. */
export function itemForEntry(items: readonly ToolRunItem<unknown>[], entryIndex: number): number | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.index > entryIndex) continue;
    const last = item.kind === "tools" ? item.index + item.calls.length - 1 : item.index;
    return entryIndex <= last ? i : null;
  }
  return null;
}

type ToolKind = { verb: string; one: string; many: string };
const READ: ToolKind = { verb: "read", one: "file", many: "files" };
const EDIT: ToolKind = { verb: "edited", one: "file", many: "files" };
const LIST: ToolKind = { verb: "listed", one: "folder", many: "folders" };
const RUN: ToolKind = { verb: "ran", one: "command", many: "commands" };
const SEARCH: ToolKind = { verb: "searched", one: "time", many: "times" };
const FETCH: ToolKind = { verb: "fetched", one: "page", many: "pages" };

/** How each built-in tool counts on a group's summary line; other tools count as "tools". */
const TOOL_KINDS: Record<string, ToolKind> = {
  read_file: READ,
  write_file: EDIT,
  edit_file: EDIT,
  list_dir: LIST,
  run_command: RUN,
  web_search: SEARCH,
  memory_search: SEARCH,
  search_sessions: SEARCH,
  web_fetch: FETCH,
};

/** A group's summary line by tool kind, in the order each kind first ran:
 * `Read 3 files, ran 2 commands, searched 1 time`. Tools without a kind count as
 * `used 2 other tools`, or `Used 4 tools` when none has one. */
export function describeToolRun(calls: readonly Pick<ToolEntry, "tool">[]): string {
  const counts = new Map<ToolKind, number>();
  let other = 0;
  for (const call of calls) {
    const kind = Object.hasOwn(TOOL_KINDS, call.tool) ? TOOL_KINDS[call.tool] : undefined;
    if (kind) counts.set(kind, (counts.get(kind) ?? 0) + 1);
    else other++;
  }
  const parts = [...counts].map(([kind, count]) => `${kind.verb} ${count} ${count === 1 ? kind.one : kind.many}`);
  if (other > 0) parts.push(`used ${other}${parts.length ? " other" : ""} ${other === 1 ? "tool" : "tools"}`);
  const text = parts.join(", ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** A group's state: running while any call runs (`current` is the latest running call), failed
 * when any call failed, otherwise succeeded. */
export function toolRunState(calls: readonly ToolEntry[]): { ok: boolean | null; current: ToolEntry | null } {
  const running = calls.filter((call) => call.ok === null);
  if (running.length > 0) return { ok: null, current: running[running.length - 1] };
  return { ok: !calls.some((call) => call.ok === false), current: null };
}
