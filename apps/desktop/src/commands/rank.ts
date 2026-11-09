// Ranking for the command palette: a fuzzy score for a query against a command's words, the
// order results are listed in (recently run first), and their grouping. Pure, so it runs under
// node's test runner (test/rank.test.ts) without React or the DOM.

/** The groups commands are listed under, in their usual order. "recent" exists only in results. */
export const GROUPS = ["recent", "actions", "sessions", "archived", "environments", "models", "panels", "settings"] as const;
export type CommandGroup = Exclude<(typeof GROUPS)[number], "recent">;
export type ResultGroup = (typeof GROUPS)[number];

export const GROUP_LABELS: Record<ResultGroup, string> = {
  recent: "Recent",
  actions: "Actions",
  sessions: "Sessions",
  archived: "Archived sessions",
  environments: "Environments",
  models: "Models",
  panels: "Panels",
  settings: "Settings",
};

/** What ranking needs from a command. */
export type Rankable = {
  id: string;
  group: CommandGroup;
  title: string;
  /** A second line (a session's environment, a model's description); matched with less weight. */
  subtitle?: string;
  /** Other words the command answers to. */
  keywords?: string[];
};

/** How many commands are remembered as recently run. */
export const RECENT_LIMIT = 20;
/** How many recent commands lead the list when nothing is typed. */
export const RECENT_SHOWN = 6;

const isBoundary = (text: string, index: number) => index === 0 || /[\s\-_:/.·,()]/.test(text[index - 1]);

/**
 * How well one typed word matches `text` (both lower case), or null when it does not: a
 * substring scores best (more at a word start, most at the start), then the letters in order
 * with bonuses for runs and word starts and a cost for gaps.
 */
function wordScore(word: string, text: string): number | null {
  const at = text.indexOf(word);
  if (at >= 0) {
    // Prefer an occurrence at a word start over an earlier one inside a word.
    let best = at;
    for (let i = at; i >= 0; i = text.indexOf(word, i + 1)) {
      if (isBoundary(text, i)) {
        best = i;
        break;
      }
    }
    return 100 + word.length * 10 + (best === 0 ? 40 : isBoundary(text, best) ? 25 : 0) - Math.min(best, 20) / 2;
  }
  let score = 0;
  let from = 0;
  let previous = -2;
  for (const char of word) {
    const found = text.indexOf(char, from);
    if (found < 0) return null;
    score += 4;
    if (found === previous + 1) score += 6;
    if (isBoundary(text, found)) score += 8;
    score -= Math.min(found - from, 6);
    previous = found;
    from = found + 1;
  }
  return Math.max(score, 1);
}

/**
 * How well `query` matches a command, or null when some typed word matches nothing. Each word
 * may match the title or, at less weight, the subtitle and keywords; the whole query found in
 * the title as typed earns a bonus, and a title that is exactly the query more.
 */
export function matchScore(query: string, command: Rankable): number | null {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return 0;
  const title = command.title.toLowerCase();
  const extra = [command.subtitle ?? "", ...(command.keywords ?? [])].join(" ").toLowerCase();
  let total = 0;
  for (const word of words) {
    const inTitle = wordScore(word, title);
    const inExtra = extra ? wordScore(word, extra) : null;
    const best = Math.max(inTitle ?? -Infinity, inExtra === null ? -Infinity : inExtra * 0.7);
    if (best === -Infinity) return null;
    total += best;
  }
  const phrase = words.join(" ");
  if (title === phrase) total += 120;
  else if (words.length > 1 && title.includes(phrase)) total += 60;
  return total;
}

/** A bonus for a recently run command: the most recent gets the most. */
function recentBonus(index: number): number {
  return index < 0 ? 0 : 80 - index * (60 / RECENT_LIMIT);
}

/**
 * The commands that match `query`, best first. With no query: the recently run ones in the
 * order they were run, then the rest as given. With one: by score, recently run ones lifted,
 * ties kept in the given order.
 */
export function rankCommands<T extends Rankable>(commands: T[], query: string, recent: string[]): T[] {
  if (!query.trim()) {
    const byId = new Map(commands.map((command) => [command.id, command]));
    const first = recent.map((id) => byId.get(id)).filter((command): command is T => command !== undefined);
    const shown = new Set(first.map((command) => command.id));
    return [...first, ...commands.filter((command) => !shown.has(command.id))];
  }
  const scored: { command: T; score: number; index: number }[] = [];
  commands.forEach((command, index) => {
    const score = matchScore(query, command);
    if (score !== null) scored.push({ command, score: score + recentBonus(recent.indexOf(command.id)), index });
  });
  scored.sort((a, b) => b.score - a.score || a.index - b.index);
  return scored.map(({ command }) => command);
}

export type Grouped<T> = { group: ResultGroup; items: T[] };

/**
 * Ranked commands under their groups. With no query, a Recent group leads (the first few
 * recently run commands, not repeated below) and the rest follow in the usual group order; with
 * one, groups follow their best match and keep the ranked order inside.
 */
export function groupResults<T extends Rankable>(ranked: T[], query: string, recent: string[]): Grouped<T>[] {
  const groups = new Map<ResultGroup, T[]>();
  const add = (group: ResultGroup, item: T) => {
    const list = groups.get(group);
    if (list) list.push(item);
    else groups.set(group, [item]);
  };
  const browsing = !query.trim();
  const leading = browsing ? new Set(recent.slice(0, RECENT_SHOWN)) : new Set<string>();
  for (const item of ranked) add(leading.has(item.id) ? "recent" : item.group, item);
  const order = browsing ? [...GROUPS] : [...groups.keys()];
  return order.filter((group) => groups.has(group)).map((group) => ({ group, items: groups.get(group)! }));
}

/** The recent list after running `id`: it moves to the front, and the oldest falls off. */
export function pushRecent(recent: string[], id: string, limit = RECENT_LIMIT): string[] {
  return [id, ...recent.filter((other) => other !== id)].slice(0, limit);
}

/** Moves through grouped results: the index of the first item of the next (or previous) group. */
export function nextGroupStart(sizes: number[], active: number, direction: 1 | -1): number {
  if (sizes.length === 0) return 0;
  const starts: number[] = [];
  let at = 0;
  for (const size of sizes) {
    starts.push(at);
    at += size;
  }
  let current = 0;
  for (let i = 0; i < starts.length; i++) if (active >= starts[i]) current = i;
  const next = (current + direction + starts.length) % starts.length;
  return starts[next];
}
