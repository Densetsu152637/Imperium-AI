// A model's reasoning as one quiet, collapsed row: "Thinking…" with a small pulse and the time so
// far while it runs, "Thought for 10 seconds" once it is done. The text is behind the button and is
// not in the page until it is opened; each block opens on its own, and nothing is remembered.

import { ChevronRight } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";

import { formatElapsed } from "../sessions/agentView";
import type { ThinkingEntry } from "../sessions/thinking";
import { liveElapsedMs, thinkingView } from "../sessions/thinking";

/** The current time, every second while `active`. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active]);
  return now;
}

export function ThinkingBlock({ entry }: { entry: ThinkingEntry }) {
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  const view = thinkingView(entry.durationMs, entry.ended, open);
  const now = useNow(view.live);
  // When this window first drew the block: a block seen live is timed from here, so a daemon on
  // another machine with a skewed clock cannot show a wrong time.
  const seen = useRef(Date.now());
  return (
    <div className={`thinking${view.live ? " live" : ""}`}>
      {/* A real button: Enter and Space toggle it, and it carries aria-expanded. The ticking time is
          hidden from screen readers so they announce only "Thinking" or "Thought for N seconds". */}
      <button
        type="button"
        className="thinking-toggle"
        aria-expanded={open}
        aria-controls={view.showText ? bodyId : undefined}
        aria-label={view.accessibleName}
        onClick={() => setOpen((was) => !was)}
      >
        <ChevronRight className="thinking-chevron" aria-hidden="true" />
        <span className="thinking-label" aria-hidden="true">
          {view.label}
        </span>
        {view.live && <span className="thinking-pulse" aria-hidden="true" />}
        {view.live && (
          <span className="thinking-elapsed" aria-hidden="true">
            {formatElapsed(liveElapsedMs(entry.startedAt, seen.current, now))}
          </span>
        )}
      </button>
      {view.showText && (
        <div className="thinking-body" id={bodyId}>
          <pre>{entry.text}</pre>
        </div>
      )}
    </div>
  );
}
