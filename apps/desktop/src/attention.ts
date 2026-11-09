// Which events get the user's attention, and how: a chime, a dot in the window title, and a
// system notification. Pure, so the rules are tested without a window (test/attention.test.ts);
// notify.ts carries them out.

import type { DownloadJob, SessionEvent } from "@imperium/protocol";

import type { AppSettings } from "./settings";

/** What a notice is about; each kind has its own switch in Settings → Notifications. */
export type Attention = "approval" | "finished" | "failed" | "budget" | "download" | "update";

/** One thing to tell the user about. */
export type Notice = {
  kind: Attention;
  title: string;
  body: string;
  /** The session it is about, opened when the user clicks the notification. */
  sessionId?: string | null;
};

/** The setting that turns system notifications of each kind on or off. */
export const SYSTEM_SETTING: Record<Attention, keyof AppSettings> = {
  approval: "notifyApproval",
  finished: "notifyFinished",
  failed: "notifyFailed",
  budget: "notifyBudget",
  download: "notifyDownload",
  update: "notifyUpdate",
};

/** Kinds that mean something waits for the user in a session, which the title badge marks. */
const NEEDS_YOU: Attention[] = ["approval", "finished", "failed", "budget"];

export type Delivery = { sound: boolean; badge: boolean; system: boolean };

/** How to deliver a notice of a kind, given the settings and whether an Imperium window has focus. */
export function deliveryFor(kind: Attention, settings: AppSettings, focused: boolean): Delivery {
  const sound =
    kind === "approval" ? settings.soundOnApproval : kind === "finished" || kind === "failed" ? settings.soundOnFinish : false;
  const badge = settings.titleBadge && !focused && NEEDS_YOU.includes(kind);
  const system = settings[SYSTEM_SETTING[kind]] === true && (!settings.notifyOnlyInBackground || !focused);
  return { sound, badge, system };
}

/** The root agent's id (sessions.ts `ROOT`; repeated so this module imports only types). */
const ROOT = "root";

/** The notice a live session event calls for, or null; `name` is the session's title. */
export function sessionNotice(sessionId: string, name: string, agentId: string, event: SessionEvent): Notice | null {
  switch (event.type) {
    case "permission_requested":
      return { kind: "approval", sessionId, title: `${name} needs approval`, body: event.summary || event.tool };
    case "questions_asked":
      return { kind: "approval", sessionId, title: `${name} has a question`, body: event.questions[0]?.question ?? "" };
    case "turn_completed":
      return agentId === ROOT ? { kind: "finished", sessionId, title: `${name} finished`, body: "The agent is waiting for you." } : null;
    case "turn_failed":
      return agentId === ROOT ? { kind: "failed", sessionId, title: `${name} failed`, body: event.message } : null;
    default:
      return null;
  }
}

/** The notice a download progress report calls for: only a finished download. */
export function downloadNotice(job: DownloadJob): Notice | null {
  return job.state === "done" ? { kind: "download", title: "Model downloaded", body: `${job.model} is ready to use.` } : null;
}

/** The notice for an update that has downloaded and installs on restart. */
export function updateNotice(version: string): Notice {
  return { kind: "update", title: "Update ready", body: `Imperium ${version} installs when you restart.` };
}
