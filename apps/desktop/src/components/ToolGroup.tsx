// Consecutive tool calls folded into one quiet line: what they did by kind ("Read 3 files, ran
// 2 commands"), a spinner and the current call while any runs, a cross when any failed. Opening it
// lists the calls as their own rows (components/ToolCall.tsx), each still opening on its own.

import { useState } from "react";

import type { ToolEntry } from "../sessions";
import { describeToolRun, toolRunState } from "../toolCalls";
import { ToolCall } from "./ToolCall";
import { ToolState } from "./ToolState";

export function ToolGroup({ calls }: { calls: ToolEntry[] }) {
  // The rows render only while open, so a long run of calls stays one line.
  const [open, setOpen] = useState(false);
  const { ok, current } = toolRunState(calls);
  return (
    <details className="tool tool-group" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        <span className="tool-state">
          <ToolState ok={ok} />
        </span>
        <span className="tool-summary">{describeToolRun(calls)}</span>
        {current && <span className="tool-current">{current.summary || current.tool}</span>}
      </summary>
      {open && (
        <div className="tool-group-calls">
          {calls.map((call, index) => (
            <ToolCall key={index} entry={call} />
          ))}
        </div>
      )}
    </details>
  );
}
