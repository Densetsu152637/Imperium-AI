// Live telemetry: one row per model in use across all sessions and subagents, updated in place
// as calls finish. The rows and their formatting live in telemetry.ts.

import { useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import type { ModelRow } from "@imperium/protocol";

import { EmptyState } from "../components/EmptyState";
import { useModelNames } from "../models/modelNames";
import type { SessionState } from "../sessions/sessions";
import { LINGER_MS, liveTelemetry, telemetryTable, withLingering, type Lingering, type TelemetryRow } from "../models/telemetry";

/** The shared table, and a hover with the full text of a cell (the model column may be cut short). */
const TABLE: Components = {
  table: ({ children }) => <table className="table compact">{children}</table>,
  td: ({ children }) => <td title={typeof children === "string" ? children : undefined}>{children}</td>,
};

export function TelemetryPanel({ sessions, models }: { sessions: SessionState[]; models: ModelRow[] }) {
  const aliases = useModelNames();
  const active = useMemo(() => liveTelemetry(sessions, models, aliases), [sessions, models, aliases]);
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
    return <EmptyState className="compact" title="No models in use" body="Models appear here while a session or subagent uses them." />;
  }
  return (
    <div className="telemetry">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={TABLE}>
        {telemetryTable(shown.rows)}
      </ReactMarkdown>
    </div>
  );
}
