// The three regions of the context bar, from 0 to the configured maximum: what the request holds
// (used), the room reserved beyond it (the pre-allocated buffer), and the rest (unallocated).

/** Where the maximum the bar runs to came from. */
export type MaxSource =
  /** The model table's `context_window` (the configured maximum). */
  | "configured"
  /** No configured maximum: the context window the model or its server reported. */
  | "window"
  /** Neither is known: the bar ends at the allocation, and nothing is left unallocated. */
  | "unknown";

export type RegionInput = {
  /** Tokens the request holds (the categories' sum). */
  used: number;
  /** The model table's `context_window`; null or absent when the row sets none. */
  configuredWindow?: number | null;
  /** What the model or its server reports it can read. */
  contextWindow?: number | null;
  /** The `max_tokens` the call was sent with: room held back for the reply. */
  reservedOutput?: number | null;
  /** Tokens of context a local server holds for the agent (its slot's `n_ctx`). */
  allocatedContext?: number | null;
};

export type Regions = {
  /** Tokens in the used region, at most the maximum. */
  used: number;
  /** Tokens beyond the maximum that the request still holds; 0 unless the request overflows. */
  overflow: number;
  /** Reserved but not yet used. */
  buffer: number;
  /** Free beyond the allocation, up to the maximum. */
  unallocated: number;
  /** `used + buffer`: used ≤ allocated ≤ max always holds. */
  allocated: number;
  /** What the bar spans: used + buffer + unallocated. */
  max: number;
  maxSource: MaxSource;
};

/** A count that can size a region: finite and above zero, else null. */
function positive(n: number | null | undefined): number | null {
  return typeof n === "number" && Number.isFinite(n) && n > 0 ? Math.floor(n) : null;
}

/**
 * Divides the context into used, buffer and unallocated. The buffer is whatever the allocation
 * holds beyond the request: the larger of the reply's reserved room and a local server's
 * allocated context less what is used. A hosted model with neither has no buffer.
 */
export function contextRegions(input: RegionInput): Regions {
  const request = Math.max(Number.isFinite(input.used) ? Math.floor(input.used) : 0, 0);
  const configured = positive(input.configuredWindow);
  const window = positive(input.contextWindow);
  const reserved = positive(input.reservedOutput) ?? 0;
  const server = positive(input.allocatedContext) ?? 0;

  const wanted = Math.max(request + reserved, server);
  const limit = configured ?? window;
  if (limit === null) {
    // Nothing says how large the context can grow, so the bar spans what is allocated.
    return {
      used: request,
      overflow: 0,
      buffer: wanted - request,
      unallocated: 0,
      allocated: wanted,
      max: wanted,
      maxSource: "unknown",
    };
  }
  const used = Math.min(request, limit);
  const allocated = Math.min(Math.max(wanted, used), limit);
  return {
    used,
    overflow: request - used,
    buffer: allocated - used,
    unallocated: limit - allocated,
    allocated,
    max: limit,
    maxSource: configured !== null ? "configured" : "window",
  };
}
