// Small pure helpers: a stable hash, which installed pet an agent gets, and text clipping.

/** FNV-1a, 32 bits: a stable number for a string. */
export function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** The `agentPets` setting: `mix` (a spread across installed pets) or one pet's id. */
export const MIX = "mix";

/**
 * The pet for something with `key`: the chosen pet when `setting` names an installed one;
 * otherwise a deterministic spread by hash, `first` (the root agent) taking the first pet.
 */
export function pickPet(ids: readonly string[], setting: string, key: string, first: boolean): string | null {
  if (ids.length === 0) return null;
  if (setting !== MIX && ids.includes(setting)) return setting;
  if (first) return ids[0] ?? null;
  return ids[hash(key) % ids.length] ?? null;
}

/** `text` cut to `max` characters with an ellipsis, whitespace collapsed. */
export function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length <= max ? flat : `${flat.slice(0, Math.max(0, max - 1)).trimEnd()}…`;
}

/** "3 subagents", "1 subagent". */
export const count = (n: number, noun: string): string => `${n} ${noun}${n === 1 ? "" : "s"}`;

/** The `agentPets` value that gives sessions the same pet as yours. */
export const MINE = "resident";

/**
 * The pet for a session: its environment's default when that pet is installed, else the nursery's
 * "Pets" setting (`mix`, `mine` = your pet, or one pet), else your pet.
 */
export function sessionPet(ids: readonly string[], envPet: string | null, setting: string, sessionId: string, resident: string | null): string | null {
  if (envPet !== null && ids.includes(envPet)) return envPet;
  if (setting !== MINE) {
    const picked = pickPet(ids, setting, sessionId, false);
    if (picked !== null) return picked;
  }
  return resident ?? ids[0] ?? null;
}
