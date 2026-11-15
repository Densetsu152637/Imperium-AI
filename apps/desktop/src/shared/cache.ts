// What the window last knew of the daemon's sessions and environments, shown until it answers.

import type { EnvironmentInfo, SessionSummary } from "@imperium/protocol";

import { newSession, type SessionState } from "../sessions";
import { getDoc } from "../store";

/** Sessions and environments as last seen, shown until the daemon answers. */
export type Cache = { sessions: SessionSummary[]; environments?: EnvironmentInfo[]; savedAt: number };

export function cachedSessions(): Record<string, SessionState> {
  const cache = getDoc<Cache>("cache");
  return Object.fromEntries(
    (cache?.sessions ?? []).map((summary, index) => [summary.session_id, newSession(summary, index)] as const),
  );
}

export const cachedEnvironments = (): EnvironmentInfo[] => getDoc<Cache>("cache")?.environments ?? [];
