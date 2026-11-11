// Live telemetry: one row per model in use across all sessions and subagents, updated in place
// as calls finish. The rows and their formatting live in telemetry.ts.

import { useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { ModelRow } from "@imperium/protocol";

import type { SessionState } from "../sessions";
import { LINGER_MS, liveTelemetry, telemetryTable, withLingering, type Lingering, type TelemetryRow } from "../telemetry";

export function TelemetryPanel({ sessions, models }: { sessions: SessionState[]; models: ModelRow[] }) {
  const active = useMemo(() => liveTelemetry(sessions, models), [sessions, models]);
  // Rows that went idle stay for a minute; a tick drops them once that passes.
  const [now, setNow] = useState(() => Date.now());
  const previous = useRef<{ rows: TelemetryRow[]; lingering: Lingering }>({ rows: [], lingering: new Map() });
  const shown = useMemo(() => {
    const next = withLingering(active, previous.current, Math.max(now, Date.now()));
    previous.current = next;
    return next;
  }, [active, now]);
  const lingering = shown.lingering.size > 0;
  useEffect(() => {
    if (!lingering) return;
    const timer = window.setInterval(() => setNow(Date.now()), Math.min(5_000, LINGER_MS));
    return () => window.clearInterval(timer);
  }, [lingering]);

  if (shown.rows.length === 0) {
    return <p className="empty">Models appear here while a session or subagent uses them.</p>;
  }
  return (
    <div className="telemetry">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{telemetryTable(shown.rows)}</ReactMarkdown>
    </div>
  );
}
