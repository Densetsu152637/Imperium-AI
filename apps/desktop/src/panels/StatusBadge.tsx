// A session's status as a badge: a mark and words as well as a colour.

import type { SessionStatus } from "@imperium/protocol";

import { Badge, type BadgeTone } from "../components/Badge";
import { formatResumeTime, type SessionState } from "../sessions/sessions";

const STATUS: Record<SessionStatus, { mark: string; label: string }> = {
  idle: { mark: "○", label: "Idle" },
  working: { mark: "●", label: "Working" },
  needs_attention: { mark: "!", label: "Needs attention" },
  paused: { mark: "⏸", label: "Paused" },
  needs_model: { mark: "?", label: "Needs a model" },
  watching: { mark: "◉", label: "Watching" },
  merged: { mark: "✓", label: "Merged" },
};

const TONE: Record<SessionStatus, BadgeTone> = {
  idle: "neutral",
  working: "accent",
  needs_attention: "danger",
  paused: "accent",
  needs_model: "accent",
  watching: "ok",
  merged: "ok",
};

/** Background sessions (started by a schedule or a project agent) say so. */
export function statusText(session: SessionState): string {
  const text = baseStatusText(session);
  return session.summary.background ? `${text} · background` : text;
}

function baseStatusText(session: SessionState): string {
  const { label } = STATUS[session.status];
  if (session.status === "paused" && session.pause && session.pause.cause !== "usage_limit") {
    return `${label} · ${session.pause.cause === "deferred" ? "waiting for a quiet machine" : "giving way to you"}`;
  }
  if (session.status === "paused" && session.pause && session.pause.untilMs > 0) {
    return `${label} · resumes ${formatResumeTime(session.pause.untilMs)}`;
  }
  if ((session.status === "watching" || session.status === "merged") && session.statusDetail) {
    return `${label} ${session.statusDetail}`;
  }
  return label;
}

export function StatusBadge({ session }: { session: SessionState }) {
  const text = statusText(session);
  return (
    <Badge tone={TONE[session.status]} className={`status ${session.status}`} title={session.pause?.reason || text}>
      <span className="status-mark" aria-hidden="true">
        {STATUS[session.status].mark}
      </span>
      {text}
    </Badge>
  );
}
