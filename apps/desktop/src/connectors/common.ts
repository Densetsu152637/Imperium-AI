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

/** Whether the text is an absolute http or https address (the form's own check, since the forms skip the browser's). */
export function isHttpUrl(text: string): boolean {
  try {
    const { protocol } = new URL(text.trim());
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

/** What is wrong with a server name, or null. `taken` only matters for a new connector. */
export function nameProblem(name: string, taken: Set<string>, editing: boolean): string | null {
  if (!NAME_PATTERN.test(name)) return "Use lowercase letters, digits, '_' or '-' for the name.";
  if (!editing && taken.has(name)) return `A connector named ${name} already exists.`;
  return null;
}

/** What is wrong with a URL field (`required` when empty is a problem), or null. */
export function urlProblem(value: string, required: boolean): string | null {
  if (!value.trim()) return required ? "Enter the server's URL." : null;
  return isHttpUrl(value) ? null : "Enter a full address starting with http:// or https://.";
}

/** The first secret variable row that has a name but neither a typed value nor a saved one, or -1. */
export function missingSecretIndex(rows: { key: string; value: string; existing: string | null }[]): number {
  return rows.findIndex((row) => row.key.trim() && !row.value && !row.existing);
}
