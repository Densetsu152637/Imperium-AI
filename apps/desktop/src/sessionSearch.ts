// Session search (`session.search`, see docs/07-desktop-ui.md#session-search): the call, the
// snippet's highlighted parts, and which transcript entry a hit opens at. The sessions sidebar
// uses these through SessionSearch.tsx; a command palette can call `searchSessions` directly.

import type { SearchHighlight, SessionSearchParams, SessionSearchResult } from "@imperium/protocol";

/** Anything that calls the daemon, such as the app's DaemonClient. */
export type SearchClient = { call<Result>(method: string, params?: unknown): Promise<Result> };

/** Searches running and archived sessions: the best match per session, best first. */
export function searchSessions(
  client: SearchClient,
  query: string,
  options: Omit<SessionSearchParams, "query"> = {},
): Promise<SessionSearchResult> {
  return client.call<SessionSearchResult>("session.search", { ...options, query });
}

export type Segment = { text: string; match: boolean };

/** A snippet split into plain and matched parts (highlights are UTF-16 ranges, as JavaScript indexes). */
export function highlightSegments(text: string, highlights: SearchHighlight[]): Segment[] {
  const segments: Segment[] = [];
  let cursor = 0;
  for (const { start, end } of [...highlights].sort((a, b) => a.start - b.start)) {
    if (start < cursor || end > text.length || end <= start) continue;
    if (start > cursor) segments.push({ text: text.slice(cursor, start), match: false });
    segments.push({ text: text.slice(start, end), match: true });
    cursor = end;
  }
  if (cursor < text.length) segments.push({ text: text.slice(cursor), match: false });
  return segments;
}

/**
 * The transcript entry holding history event `event`: the last entry that starts at or before it
 * (a reply's later chunks, and subagents' tool calls, fall inside an earlier entry). `starts`
 * are the entries' `event` marks in order; null when no entry starts that early.
 */
export function entryForEvent(starts: (number | undefined)[], event: number): number | null {
  let found: number | null = null;
  starts.forEach((start, index) => {
    if (start !== undefined && start <= event) found = index;
  });
  return found;
}

/** What a hit matched, for its label. */
export function hitKindLabel(kind: string): string {
  switch (kind) {
    case "user":
      return "You";
    case "assistant":
      return "Reply";
    case "tool":
      return "Tool";
    case "task":
      return "Subagent brief";
    case "report":
      return "Subagent report";
    case "title":
      return "Title";
    default:
      return kind;
  }
}
