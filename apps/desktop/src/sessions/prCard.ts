// The pull-request card's rules, kept free of React so `node --test` can check them
// (test/prCard.test.ts). The daemon's `PrStatus` is the only source; every string in it came from
// GitHub, so it is cleaned here (`plain`) and the card draws it only as text, never as markup or a
// link target. The link is the one exception, and only when it is the daemon's own
// `https://github.com/<owner>/<repo>/pull/<n>` form.

import type { PrMergeMethod, PrStatus } from "@imperium/protocol";

import { plain } from "../shared/plainText.ts";

export type Tone = "neutral" | "accent" | "ok" | "warn" | "danger";

/** Which glyph the state badge shows. */
export type StateIcon = "open" | "draft" | "merged" | "closed";

export type CheckView = {
  /** The headline: `2 failing`, `1 running`, `All 5 passing`, `No checks`. */
  label: string;
  /** The counts, `5 passed, 2 failed, 1 running`; empty when there are no checks. */
  detail: string;
  tone: Tone;
  /** Drives the `ToolState` icon: null while checks run, true when all pass, false when one failed; undefined for none. */
  ok: boolean | null | undefined;
  failing: string[];
  /** Failing checks beyond the ones listed. */
  more: number;
};

export type SwitchView = {
  /** The switch's accessible name. */
  label: string;
  on: boolean;
  /** What the switch does, or why it cannot be changed (the auto-fix switch is not built yet). */
  reason: string;
  /** What GitHub or the repository says about it for this pull request, when anything. */
  hint: string | null;
  /** Whether turning it on is offered now. */
  canTurnOn: boolean;
  /** Whether turning it off is offered now. */
  canTurnOff: boolean;
};

/** The user's `github.automerge.*` settings, as the card needs them. */
export type AutoMergeSettings = {
  /** `github.automerge.allowed`: false refuses every request to turn auto-merge on. */
  allowed: boolean;
  /** `github.automerge.default_method`: preselected when the repository allows it; never turns anything on. */
  preferred: PrMergeMethod | null;
};

export const DEFAULT_AUTO_MERGE_SETTINGS: AutoMergeSettings = { allowed: true, preferred: null };

const METHODS: readonly PrMergeMethod[] = ["squash", "merge", "rebase"];

/** The settings from `config.describe` entries; anything missing or odd is the default. */
export function autoMergeSettings(entries: ReadonlyArray<{ key: string; value: unknown }>): AutoMergeSettings {
  const value = (key: string): unknown => entries.find((entry) => entry.key === key)?.value;
  const preferred = value("github.automerge.default_method");
  return {
    allowed: value("github.automerge.allowed") !== false,
    preferred: METHODS.find((method) => method === preferred) ?? null,
  };
}

/** The method the picker preselects: the preference when the repository allows it, else the first allowed of squash, merge, rebase. */
export function chooseMethod(allowed: readonly PrMergeMethod[], preferred: PrMergeMethod | null): PrMergeMethod | null {
  if (preferred && allowed.includes(preferred)) return preferred;
  return METHODS.find((method) => allowed.includes(method)) ?? null;
}

export type MethodOption = { value: PrMergeMethod; label: string };

export type PrCardView = {
  title: string;
  /** `owner/repo#12`. */
  ref: string;
  /** The daemon's pull request address when it has the expected form; null otherwise. */
  link: string | null;
  /** `branch into base`. */
  branches: string;
  state: { label: string; tone: Tone; icon: StateIcon };
  checks: CheckView;
  review: { label: string; tone: Tone };
  merge: { label: string; tone: Tone };
  /** The merge-state note (`Behind its base`), when it says more than the other lines. */
  mergeNote: string | null;
  /** Merged or closed: nothing more will change. */
  ended: boolean;
  autoFix: SwitchView;
  autoMerge: SwitchView;
  methods: MethodOption[];
  /** The method the picker shows, from the request or the first allowed one. */
  method: PrMergeMethod | null;
  /** Said when the repository's merge methods could not be read or the requested one is not allowed. */
  methodNote: string | null;
  /** Why the last poll failed, with the status below it kept; null when it worked. */
  error: string | null;
};

const MAX_FAILING = 5;
const PULL_REQUEST_LINK = /^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/pull\/\d+$/;

/** GitHub text on one line with control and direction characters removed. */
export const oneLine = (text: string | null | undefined): string => plain(text).replace(/\s+/g, " ");

const METHOD_LABELS: Record<PrMergeMethod, string> = {
  squash: "Squash and merge",
  merge: "Create a merge commit",
  rebase: "Rebase and merge",
};

/** The pull request address when it is the daemon's own form; anything else is not made a link. */
export function safeLink(url: string): string | null {
  return PULL_REQUEST_LINK.test(url) ? url : null;
}

/** `owner/repo#12` from a pull request address, for an archived session's row. */
export function refOfUrl(url: string): string {
  const match = /^https:\/\/github\.com\/([^/]+\/[^/]+)\/pull\/(\d+)/.exec(url);
  return match ? `${oneLine(match[1])}#${match[2]}` : oneLine(url);
}

function describeChecks(checks: PrStatus["checks"]): CheckView {
  const failing = checks.failing_names.map(oneLine).filter(Boolean);
  const listed = failing.slice(0, MAX_FAILING);
  const more = Math.max(checks.failed - listed.length, 0);
  const parts = [
    checks.passed > 0 ? `${checks.passed} passed` : "",
    checks.failed > 0 ? `${checks.failed} failed` : "",
    checks.pending > 0 ? `${checks.pending} running` : "",
  ].filter(Boolean);
  const detail = parts.join(", ");
  if (checks.failed > 0) return { label: `${checks.failed} failing`, detail, tone: "danger", ok: false, failing: listed, more };
  if (checks.pending > 0) return { label: `${checks.pending} running`, detail, tone: "warn", ok: null, failing: [], more: 0 };
  if (checks.passed > 0) return { label: `All ${checks.passed} passing`, detail, tone: "ok", ok: true, failing: [], more: 0 };
  return { label: "No checks", detail: "", tone: "neutral", ok: undefined, failing: [], more: 0 };
}

const REVIEW: Record<PrStatus["review"], { label: string; tone: Tone }> = {
  approved: { label: "Approved", tone: "ok" },
  changes_requested: { label: "Changes requested", tone: "danger" },
  review_required: { label: "Review required", tone: "warn" },
  none: { label: "No review yet", tone: "neutral" },
};

const MERGEABLE: Record<PrStatus["mergeable"], { label: string; tone: Tone }> = {
  mergeable: { label: "No conflicts", tone: "ok" },
  conflicting: { label: "Has conflicts", tone: "danger" },
  unknown: { label: "Checking for conflicts", tone: "neutral" },
};

const STATE: Record<PrStatus["state"], { label: string; tone: Tone; icon: StateIcon }> = {
  open: { label: "Open", tone: "ok", icon: "open" },
  draft: { label: "Draft", tone: "neutral", icon: "draft" },
  merged: { label: "Merged", tone: "accent", icon: "merged" },
  closed: { label: "Closed", tone: "danger", icon: "closed" },
};

/** What GitHub's merge state adds beyond the review, check and conflict lines. */
const MERGE_NOTES: Record<string, string> = {
  queued: "In the merge queue",
  behind: "Behind its base branch",
  blocked: "Blocked by the repository's rules",
  unstable: "Merging is allowed but a check has not passed",
  has_hooks: "Waiting on the repository's merge checks",
};

const AUTO_FIX_PENDING = "Fixing from review comments is not available yet.";
const AUTO_MERGE_OFF = "GitHub will merge it when its own rules pass, with nobody looking at the code again. Imperium never merges it itself.";
const AUTO_MERGE_ON = "GitHub merges it when its checks and reviews pass. Imperium never merges it itself, and turns this off before it pushes to the branch.";
const AUTO_MERGE_SETTING_OFF = "Auto-merge is turned off in your settings (github.automerge.allowed).";
const METHODS_UNKNOWN = "The repository's merge methods could not be read.";

export const DRAFT_HINT = "GitHub does not allow auto-merge on a draft. Mark the pull request ready for review first.";

function autoMergeHint(status: PrStatus): string | null {
  if (status.state === "merged") return "This pull request is already merged.";
  if (status.state === "closed") return "This pull request is closed.";
  if (status.state === "draft" || status.draft) return DRAFT_HINT;
  const reason = oneLine(status.auto_merge.reason_unavailable);
  return reason || null;
}

function autoMergeView(status: PrStatus, settings: AutoMergeSettings): SwitchView {
  const on = status.auto_merge.enabled;
  const ended = status.state === "merged" || status.state === "closed";
  let hint = autoMergeHint(status);
  if (!hint && !settings.allowed) hint = AUTO_MERGE_SETTING_OFF;
  if (!hint && status.allowed_methods.length === 0) hint = METHODS_UNKNOWN;
  return {
    label: "Auto-merge",
    on,
    reason: on ? AUTO_MERGE_ON : AUTO_MERGE_OFF,
    hint,
    canTurnOn: !on && !hint,
    canTurnOff: on && !ended,
  };
}

function methodView(status: PrStatus, settings: AutoMergeSettings): Pick<PrCardView, "methods" | "method" | "methodNote"> {
  const methods = status.allowed_methods.map((value) => ({ value, label: METHOD_LABELS[value] }));
  const requested = status.auto_merge.method ?? null;
  if (methods.length === 0) {
    return { methods, method: requested, methodNote: "The repository's merge methods could not be read." };
  }
  if (requested && !status.allowed_methods.includes(requested)) {
    return { methods, method: requested, methodNote: `${METHOD_LABELS[requested]} is not allowed in this repository.` };
  }
  return { methods, method: requested ?? chooseMethod(status.allowed_methods, settings.preferred), methodNote: null };
}

/** Everything the card draws, from one `PrStatus`. */
export function describePr(status: PrStatus, settings: AutoMergeSettings = DEFAULT_AUTO_MERGE_SETTINGS): PrCardView {
  const state = status.state === "open" && status.draft ? STATE.draft : STATE[status.state];
  const ended = status.state === "merged" || status.state === "closed";
  const branch = oneLine(status.branch);
  const base = oneLine(status.base);
  return {
    title: oneLine(status.title) || "(untitled)",
    ref: `${oneLine(status.repo)}#${status.number}`,
    link: safeLink(status.url),
    branches: branch && base ? `${branch} into ${base}` : branch || base,
    state,
    checks: describeChecks(status.checks),
    review: REVIEW[status.review],
    merge: MERGEABLE[status.mergeable],
    mergeNote: MERGE_NOTES[status.merge_state] ?? null,
    ended,
    autoFix: { label: "Fix CI and review comments", on: status.auto_fix, reason: AUTO_FIX_PENDING, hint: null, canTurnOn: false, canTurnOff: false },
    autoMerge: autoMergeView(status, settings),
    ...methodView(status, settings),
    error: status.error ? oneLine(status.error) : null,
  };
}

/**
 * The status to show after a daemon answer. A `pull_requests.changed` notification always wins; a
 * `pull_requests.status` answer that arrives after a newer notification does not replace it.
 */
export function nextStatus(current: PrStatus | null, incoming: PrStatus | null, fromNotification: boolean): PrStatus | null {
  if (fromNotification || !current || !incoming) return incoming;
  return incoming.updated_at >= current.updated_at ? incoming : current;
}

export type BindInput = { ok: true; url: string } | { ok: false; reason: string };

const BIND_URL = /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/pull\/(\d+)(?:\/(?:files|commits|checks))?\/?(?:[?#].*)?$/;

/** Checks what was typed in the bind field, so a wrong address is refused before it reaches the daemon. */
export function parseBindInput(text: string): BindInput {
  const trimmed = text.trim();
  if (!trimmed) return { ok: false, reason: "Paste the pull request's address." };
  const match = BIND_URL.exec(trimmed);
  if (!match) return { ok: false, reason: "Use an address like https://github.com/owner/repo/pull/12." };
  return { ok: true, url: `https://github.com/${match[1]}/${match[2]}/pull/${match[3]}` };
}

/** The daemon's answer while a fresh binding awaits its first read: not a failure. */
export const isFirstRead = (reason: string): boolean => reason.startsWith("the pull request has not been read yet");
