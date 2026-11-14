// The first-run tour's data and logic, free of React and the DOM so it can be tested (test/tourSteps.test.ts).
// Steps are declarative: add an entry to TOUR_STEPS and it appears in the tour. A step with a
// `target` (a CSS selector) points at that element and is skipped while the element is not on
// screen (no session open yet, a panel closed, a detached window); a step without one is shown
// centred. Whether the user has seen the tour is kept in the app settings (`tour`), so it is
// remembered per user and shows once. The tour needs no network.

import type { AppSettings } from "./settings";

export type TourStep = {
  id: string;
  title: string;
  /** What the step says: one or two plain sentences. */
  body: string;
  /** A CSS selector for the element to point at; omitted for a step about the app as a whole. */
  target?: string;
};

/** How the tour last ended for this user; null while it has never run. */
export type TourOutcome = "finished" | "skipped";
/** "existing" marks an install that predates the tour: it never starts by itself. */
export type TourState = { outcome: TourOutcome | "existing"; at: string } | null;

export const TOUR_STEPS: TourStep[] = [
  {
    id: "welcome",
    title: "Welcome to Imperium",
    body: "A short tour of the main areas. Use the arrow keys or Next and Back to move, and Esc to skip it; you can take it again any time from Settings → About or the command palette.",
  },
  {
    id: "environments",
    title: "Environments and Home",
    body: "An environment is a project: its folders, runtime, agents, and memory. Home holds work that belongs to no environment, such as your own notes and automations.",
    target: '[data-panel="projects"]',
  },
  {
    id: "sessions",
    title: "Sessions",
    body: "Each conversation with an agent is a session. They are listed here by environment; pin, archive, or search them, and pick one up where you left off.",
    target: '[data-panel="sessions"]',
  },
  {
    id: "composer",
    title: "The composer",
    body: "Write a message here and send it. Attach files, switch the model, and queue messages while the agent is working.",
    target: "form.composer",
  },
  {
    id: "layout",
    title: "Panels and layouts",
    body: "Every area is a panel. Edit the layout to move panels between columns, stack them, or pop one out into its own window; Settings → Panels reopens a closed one.",
    target: '[data-tour="layout"]',
  },
  {
    id: "palette",
    title: "The command palette",
    body: "Press Ctrl+K (Cmd+K on macOS, or the key you chose in Settings → Keyboard) to search every action, session, environment, model, panel, and settings section from the keyboard.",
  },
  {
    id: "approvals",
    title: "Permissions and approvals",
    body: "Agents ask before they change things. Approvals appear in their own panel and as a notification; Settings → Permissions sets what is always allowed, asked, or refused.",
  },
  {
    id: "memory",
    title: "Memory",
    body: "Imperium can remember facts across sessions, per environment or for you, and shows what it saved so you can edit or delete it. Memory settings live under Settings.",
  },
  {
    id: "settings",
    title: "Settings",
    body: "Appearance, models, notifications, budgets, and the daemon are all here. The search box finds any setting by name.",
    target: '[data-tour="settings"]',
  },
];

/** True when the tour should start by itself: it has never run for this user. */
export function shouldAutoStart(settings: Pick<AppSettings, "tour">): boolean {
  return settings.tour == null;
}

/** What the app looks like right now, for deciding whether to start the tour. */
export type AppReadiness = {
  /** Connected to imperium-core. */
  connected: boolean;
  /** The main interface is on screen (the sessions panel exists). */
  uiReady: boolean;
  /** A dialog, the palette, or a confirmation is open. */
  modalOpen: boolean;
  /** This is the main window, not a panel window. */
  mainWindow: boolean;
};

/** The tour starts by itself once, for a new user, when the main interface is up and nothing else is open. */
export function autoStartReady(settings: Pick<AppSettings, "tour">, app: AppReadiness): boolean {
  return shouldAutoStart(settings) && app.mainWindow && app.connected && app.uiReady && !app.modalOpen;
}

/**
 * What to record for a settings document just loaded, or undefined to leave it: a document that
 * exists without a `tour` entry belongs to an install that predates the tour, so it is marked
 * "existing" and the tour does not start by itself (it can still be taken from the palette).
 */
export function migrateTour(saved: object | undefined, now: Date = new Date()): TourState | undefined {
  if (!saved || "tour" in saved) return undefined;
  return { outcome: "existing", at: now.toISOString() };
}

/** The settings after the tour ended (finished on its last step, or skipped). */
export function recordOutcome<T extends Pick<AppSettings, "tour">>(settings: T, outcome: TourOutcome, now: Date = new Date()): T {
  return { ...settings, tour: { outcome, at: now.toISOString() } };
}

/** Steps that can be shown now: those without a target, or whose target exists. */
export function availableSteps(steps: TourStep[], exists: (selector: string) => boolean): TourStep[] {
  return steps.filter((step) => !step.target || exists(step.target));
}

/**
 * Where the tour stands: the available steps and the index of the current one. If the current
 * step is gone (its target disappeared) the next available one is used, else the last; `null`
 * when no step is available at all.
 */
export function settleStep(
  steps: TourStep[],
  currentId: string,
  exists: (selector: string) => boolean,
): { steps: TourStep[]; index: number } | null {
  const shown = availableSteps(steps, exists);
  if (shown.length === 0) return null;
  const direct = shown.findIndex((step) => step.id === currentId);
  if (direct >= 0) return { steps: shown, index: direct };
  const at = steps.findIndex((step) => step.id === currentId);
  const after = shown.findIndex((step) => steps.indexOf(step) > at);
  return { steps: shown, index: after >= 0 ? after : shown.length - 1 };
}

/** The step to show after `currentId`, or null when it was the last (the tour is finished). */
export function nextStep(steps: TourStep[], currentId: string, exists: (selector: string) => boolean): TourStep | null {
  const at = steps.findIndex((step) => step.id === currentId);
  return availableSteps(steps.slice(at + 1), exists)[0] ?? null;
}

/** The step before `currentId`, or null when it is the first available one. */
export function previousStep(steps: TourStep[], currentId: string, exists: (selector: string) => boolean): TourStep | null {
  const at = steps.findIndex((step) => step.id === currentId);
  const before = availableSteps(steps.slice(0, Math.max(at, 0)), exists);
  return before[before.length - 1] ?? null;
}

export type Box = { left: number; top: number; width: number; height: number };

/**
 * Where the step's card goes: beside the highlighted element (below it, else above, else to
 * the side), kept inside the viewport; near the middle when there is no element.
 */
export function cardPosition(
  target: Box | null,
  card: { width: number; height: number },
  viewport: { width: number; height: number },
  gap = 12,
): { left: number; top: number } {
  const clamp = (value: number, size: number, limit: number) => Math.max(gap, Math.min(value, limit - size - gap));
  if (!target) return { left: (viewport.width - card.width) / 2, top: Math.max(gap, (viewport.height - card.height) / 3) };
  const centreX = target.left + target.width / 2 - card.width / 2;
  if (target.top + target.height + gap + card.height + gap <= viewport.height) {
    return { left: clamp(centreX, card.width, viewport.width), top: target.top + target.height + gap };
  }
  if (target.top - gap - card.height >= gap) {
    return { left: clamp(centreX, card.width, viewport.width), top: target.top - gap - card.height };
  }
  const right = target.left + target.width + gap;
  const left = right + card.width + gap <= viewport.width ? right : target.left - gap - card.width;
  return { left: clamp(left, card.width, viewport.width), top: clamp(target.top, card.height, viewport.height) };
}
