// A session's status as a badge: a mark and words as well as a colour.

import type { SessionStatus } from "@imperium/protocol";

import { formatResumeTime, type SessionState } from "../sessions";

const STATUS: Record<SessionStatus, { mark: string; label: string }> = {
  idle: { mark: "○", label: "Idle" },
  working: { mark: "●", label: "Working" },
  needs_attention: { mark: "!", label: "Needs attention" },
  paused: { mark: "⏸", label: "Paused" },
  watching: { mark: "◉", label: "Watching" },
  merged: { mark: "✓", label: "Merged" },
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
    <span className={`badge status ${session.status}`} title={session.pause?.reason || text}>
      <span className="status-mark" aria-hidden="true">
        {STATUS[session.status].mark}
      </span>
      {text}
    </span>
  );
}
