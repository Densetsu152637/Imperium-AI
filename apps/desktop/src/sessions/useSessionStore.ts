// The sessions the window knows, which one is open, and what the main panel shows in its place
// (a subagent's log, a comparison). The daemon connection fills the sessions in (App.tsx).

import { useMemo, useRef, useState } from "react";

import { cachedSessions } from "../shared/cache";
import type { SessionState } from "../sessions";

/** `initialSelected`: the session the layout last had open, when it is restored. */
export function useSessionStore(initialSelected: string | null) {
  const [sessions, setSessions] = useState<Record<string, SessionState>>(cachedSessions);
  const [selected, setSelected] = useState<string | null>(initialSelected);
  /** A subagent whose log the main panel shows in place of its session's transcript. */
  const [agentView, setAgentView] = useState<{ sessionId: string; agentId: string } | null>(null);
  /** The comparison the main panel shows side by side in place of a session (`session.compare`). */
  const [compareView, setCompareView] = useState<string | null>(null);
  /** A message branched from, handed to the new session's composer to edit. */
  const [prefill, setPrefill] = useState<{ sessionId: string; text: string; nonce: number } | null>(null);
  // The latest sessions, for the daemon's notification handler (set up once per connection).
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;

  const allSessions = useMemo(() => Object.values(sessions), [sessions]);
  const session = selected ? (sessions[selected] ?? null) : null;
  const openAgent = session && agentView?.sessionId === selected ? agentView.agentId : null;
  const viewedTask = openAgent ? (session?.tasks[openAgent] ?? null) : null;
  return {
    sessions,
    setSessions,
    sessionsRef,
    selected,
    setSelected,
    agentView,
    setAgentView,
    compareView,
    setCompareView,
    prefill,
    setPrefill,
    allSessions,
    session,
    openAgent,
    viewedTask,
  };
}
