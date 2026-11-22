// `@` file mentions in the composer. Typing `@` after a space (or at the start) opens a picker over
// the session's files (`session.files`); choosing one puts its path in the message as `@path` and
// adds a chip: a file is staged with `session.attach` (`mention: true`, so the daemon refuses
// anything outside the session's folders), a directory is only named. The functions here are pure so
// they can be tested without a DOM.

import type { Chip } from "./attachments";

/** The `@word` the caret is in. */
export type ActiveMention = {
  /** Index of the `@`. */
  start: number;
  /** Index just past the word, so choosing replaces what was typed after the caret too. */
  end: number;
  /** What was typed after the `@`, up to the caret. */
  query: string;
};

/** The mention the caret is in, or null: an `@` that starts a word, with no whitespace up to the caret. */
export function activeMention(text: string, caret: number): ActiveMention | null {
  const before = text.slice(0, caret);
  const start = before.lastIndexOf("@");
  if (start < 0) return null;
  // `name@example.com` is an address, not a mention.
  if (start > 0 && !/\s/.test(before[start - 1])) return null;
  const query = before.slice(start + 1);
  if (/\s/.test(query)) return null;
  let end = caret;
  while (end < text.length && !/\s/.test(text[end])) end += 1;
  return { start, end, query };
}

/** How a path is written in a message: `@src/lib.rs`, or `@"my notes.md"` when it has spaces. */
export function mentionToken(relative: string): string {
  return /\s/.test(relative) ? `@"${relative}"` : `@${relative}`;
}

/** Replaces the typed `@query` with the chosen path and a space; the caret goes after the space. */
export function insertMention(text: string, active: ActiveMention, relative: string): { text: string; caret: number } {
  const token = mentionToken(relative);
  const before = text.slice(0, active.start);
  const after = text.slice(active.end);
  const tail = after.startsWith(" ") ? after : ` ${after}`;
  return { text: `${before}${token}${tail}`, caret: before.length + token.length + 1 };
}

/** Whether a token ends at `end`: at the end or before a space, with sentence punctuation (`, . ; : ! ? )`) allowed between. */
function endsWord(text: string, end: number): boolean {
  let at = end;
  while (at < text.length && ",.;:!?)".includes(text[at])) at += 1;
  return at === text.length || /\s/.test(text[at]);
}

/** Where `token` stands in `text` as a whole word, or -1 (`@src/a` is not in `@src/ab`). */
function find(text: string, token: string): number {
  let from = 0;
  for (;;) {
    const at = text.indexOf(token, from);
    if (at < 0) return -1;
    if ((at === 0 || /\s/.test(text[at - 1])) && endsWord(text, at + token.length)) return at;
    from = at + 1;
  }
}

/** Takes a mention's token (and a space beside it) out of the message, as when its chip is removed. */
export function withoutMention(text: string, token: string): string {
  const at = find(text, token);
  if (at < 0) return text;
  const end = at + token.length;
  if (text[end] === " ") return text.slice(0, at) + text.slice(end + 1);
  if (at > 0 && text[at - 1] === " ") return text.slice(0, at - 1) + text.slice(end);
  return text.slice(0, at) + text.slice(end);
}

/** The chips that are still wanted: those of a mention whose token is still in the message, and all others. */
export function syncMentions(chips: Chip[], text: string): Chip[] {
  return chips.filter((chip) => chip.mention === undefined || find(text, chip.mention) >= 0);
}

/**
 * Tells the model where a mentioned directory is: a directory is never attached, and `@docs` alone
 * does not say which of the session's folders it is in.
 */
export function mentionNotes(chips: Chip[]): string {
  const notes = chips.flatMap((chip) =>
    chip.state === "ready" && chip.directory && chip.mention ? [`${chip.mention} is the folder ${chip.directory}`] : [],
  );
  return notes.length === 0 ? "" : `\n\n(${notes.join("; ")})`;
}

/** The message text to send: what was typed, with the notes for mentioned directories. */
export function composeMessage(text: string, chips: Chip[]): string {
  return `${text.trim()}${mentionNotes(chips)}`.trim();
}
