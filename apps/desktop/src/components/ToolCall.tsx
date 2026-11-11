// One tool call as a quiet, collapsed row: state, tool, what it does, and how long it took.
// Opening it shows the arguments and the result; long text folds behind "show more".

import { useState } from "react";

import type { ToolEntry } from "../sessions";
import { clipText, formatToolDuration, prettyArguments } from "../toolCalls";
import { ToolState } from "./ToolState";

export function ToolCall({ entry }: { entry: ToolEntry }) {
  // The body renders only while open, so a long transcript keeps one summary line per call.
  const [open, setOpen] = useState(false);
  const args = prettyArguments(entry.arguments);
  const row = (
    <>
      <span className="tool-state">
        <ToolState ok={entry.ok} />
      </span>
      {entry.tool && <span className="tool-name">{entry.tool}</span>}
      <span className="tool-summary">{entry.summary}</span>
      {entry.durationMs !== null && <span className="tool-duration">{formatToolDuration(entry.durationMs)}</span>}
    </>
  );
  const failed = entry.ok === false ? " failed" : "";
  if (!args && !entry.output) {
    return <div className={`tool plain${failed}`}>{row}</div>;
  }
  return (
    <details className={`tool${failed}`} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>{row}</summary>
      {open && (
        <div className="tool-body">
          {args && <ToolText label="Arguments" text={args} />}
          {entry.output && <ToolText label="Result" text={entry.output} />}
        </div>
      )}
    </details>
  );
}

function ToolText({ label, text }: { label: string; text: string }) {
  const [whole, setWhole] = useState(false);
  const clipped = whole ? null : clipText(text);
  return (
    <div className="tool-text">
      <div className="tool-label">{label}</div>
      <pre>{clipped ?? text}</pre>
      {clipped !== null && (
        <button type="button" className="link tool-more" onClick={() => setWhole(true)}>
          Show more
        </button>
      )}
    </div>
  );
}
