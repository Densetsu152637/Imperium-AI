// The status line under a sent message (and in the Agents panel's rows) while the core works
// before the agent's first output: what it waits on (waits.ts), how long it has waited, and
// the action that ends or answers the wait.

import { LoaderCircle, Play, Square } from "lucide-react";
import { useEffect, useState } from "react";

import { formatElapsed, type WaitView } from "../waits";
import { modelAliases } from "../modelNames";

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

export type WaitHandlers = {
  /** Stops the session's work (`session.control` `stop`). */
  onStop?: () => void;
  /** Resumes a stopped session. */
  onResume?: () => void;
  /** Ends a usage-limit or quiet-machine wait now. */
  onResumeNow?: () => void;
  /** Answers a budget question: send anyway (`true`) or drop the message. */
  onBudget?: (confirm: boolean) => void;
};

/** The model a wait is about, for its hover text: "Qwen 3 (4B) (llamacpp:Qwen3-4B…)" when it has an alias. */
function modelLabel(model: string): string {
  const alias = modelAliases().get(model);
  return alias ? `${alias} (${model})` : model;
}

export function WaitLine({ view, compact = false, ...handlers }: { view: WaitView; compact?: boolean } & WaitHandlers) {
  const now = useNow(true);
  const elapsed = formatElapsed(now - view.since);
  return (
    <div className={`wait-line${compact ? " compact" : ""}`} role="status" title={view.model ? modelLabel(view.model) : undefined}>
      <LoaderCircle className="wait-spinner" aria-hidden="true" />
      <span className="wait-text">{view.text}</span>
      <span className="wait-elapsed muted" aria-label={`waiting for ${elapsed}`}>
        {elapsed}
      </span>
      {view.action === "stop" && handlers.onStop && (
        <button type="button" className="ghost danger" onClick={handlers.onStop} title="Stop: pause the agent and its subagents at their next safe point (Ctrl+.)">
          <Square aria-hidden="true" /> Stop
        </button>
      )}
      {view.action === "resume" && handlers.onResume && (
        <button type="button" className="ghost" onClick={handlers.onResume}>
          <Play aria-hidden="true" /> Resume
        </button>
      )}
      {view.action === "resume_now" && handlers.onResumeNow && (
        <button type="button" className="ghost" onClick={handlers.onResumeNow}>
          Resume now
        </button>
      )}
      {view.action === "budget" && handlers.onBudget && (
        <>
          <button type="button" className="primary" onClick={() => handlers.onBudget?.(true)}>
            Send anyway
          </button>
          <button type="button" className="ghost" onClick={() => handlers.onBudget?.(false)}>
            Don&apos;t send
          </button>
        </>
      )}
    </div>
  );
}
