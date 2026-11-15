// Whether a session's model reasons, and so whether the effort control is live (docs/07).
// The daemon reports it per session (`SessionSummary.reasoning`); a session with no model yet
// cannot reason. Run its tests with `pnpm test`.

/** The tooltip on the effort control while the model cannot reason. */
export const NO_REASONING_HINT = "This model doesn't reason";

/** Why the effort control is unavailable: no model yet, or one that does not reason. */
export function effortHint(hasModel: boolean): string {
  return hasModel ? NO_REASONING_HINT : "Choose a model first";
}

/** Whether the session's model can reason; false while it has no model. */
export function reasoningAvailable(summary: { model: string | null; reasoning: boolean }): boolean {
  return summary.model !== null && summary.reasoning;
}

/** The effort to send to the daemon: none for a model that cannot reason, or when unchanged. */
export function effortToSend<E extends string>(canReason: boolean, current: E | undefined, next: E): E | null {
  if (!canReason || next === current) return null;
  return next;
}
