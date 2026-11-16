// Small helpers the Connectors views share.

export const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function matches(query: string, ...texts: (string | null | undefined)[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = texts.filter(Boolean).join(" ").toLowerCase();
  return words.every((word) => haystack.includes(word));
}

export const count = (n: number, noun: string) => (n > 0 ? `${n} ${noun}${n === 1 ? "" : "s"}` : null);

// ---- Forms ----

/** Server names the daemon accepts: lowercase letters, digits, '_' and '-'. */
export const NAME_PATTERN = /^[a-z0-9][a-z0-9_-]{0,39}$/;
