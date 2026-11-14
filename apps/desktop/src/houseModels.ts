// The house models: Emperor and Consul, Imperium's own fine-tuned open-weight models. On a first
// run with no model at all the new-session view suggests them (panels/HouseModelCard.tsx). They
// are still in training, so no entry has a download source yet; the card says so instead of
// offering a button that goes nowhere. Pure, so the catalogue's shape and the rule for when the
// card shows are tested without a window (test/houseModels.test.ts).

/** Where a house model can be downloaded from: a Hugging Face repository and, optionally, a quantisation. */
export type HouseModelSource = { repo: string; quant?: string };

export type HouseModel = {
  id: string;
  name: string;
  /** One or two sentences on what the model is for. */
  role: string;
  /** Approximate download size in bytes, or null when not known. */
  approxBytes: number | null;
  /** Absent while the model is still in training. */
  source: HouseModelSource | null;
};

const GB = 1_000_000_000;

/** The catalogue as shipped. Setting an entry's `source` is all it takes to offer its download. */
export const HOUSE_CATALOGUE_RAW: unknown = [
  {
    id: "emperor",
    name: "Emperor",
    role: "The root agent and planner: turns a request into a staged plan and runs it through subagents.",
    approx_bytes: 2.7 * GB,
    source: null,
  },
  {
    id: "consul",
    name: "Consul",
    role: "Emperor's partner: a larger model for the work that needs more depth than the planner.",
    approx_bytes: 5.5 * GB,
    source: null,
  },
];

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** `owner/name`, as Hugging Face repositories are written. */
const REPO = /^[A-Za-z0-9][\w.-]*\/[\w.-]+$/;

function parseSource(raw: unknown): HouseModelSource | null {
  if (!isRecord(raw) || typeof raw.repo !== "string" || !REPO.test(raw.repo)) return null;
  const quant = typeof raw.quant === "string" && raw.quant.trim() ? raw.quant.trim() : undefined;
  return quant ? { repo: raw.repo, quant } : { repo: raw.repo };
}

/**
 * Reads a catalogue: an array of `{ id, name, role, approx_bytes?, source? }`. Entries without
 * an id, name or role are dropped, as are repeated ids; a malformed source means "no source",
 * so a bad entry can never produce a download button.
 */
export function parseHouseCatalogue(raw: unknown): HouseModel[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const models: HouseModel[] = [];
  for (const item of raw) {
    if (!isRecord(item)) continue;
    const { id, name, role } = item;
    if (typeof id !== "string" || !id || typeof name !== "string" || !name || typeof role !== "string" || !role) continue;
    if (seen.has(id)) continue;
    seen.add(id);
    const bytes = item.approx_bytes;
    models.push({
      id,
      name,
      role,
      approxBytes: typeof bytes === "number" && Number.isFinite(bytes) && bytes > 0 ? bytes : null,
      source: parseSource(item.source),
    });
  }
  return models;
}

export const HOUSE_MODELS: HouseModel[] = parseHouseCatalogue(HOUSE_CATALOGUE_RAW);

/** "about 2.7 GB", or "size not known yet". */
export function approxSize(bytes: number | null): string {
  if (bytes === null) return "size not known yet";
  return `about ${(bytes / GB).toFixed(1)} GB`;
}

/** What the daemon reported about the models the user has. */
export type ModelInventory = {
  /** Hosted table rows with an endpoint configured. */
  hostedConfigured: number;
  /** Local models found in the model folders, usable or not. */
  localFound: number;
  /** `models.default`, when set. */
  defaultModel: string | null;
};

/** True when the user has any model at all: then the suggestion never shows. */
export const hasAnyModel = (inventory: ModelInventory): boolean =>
  inventory.hostedConfigured > 0 || inventory.localFound > 0 || inventory.defaultModel !== null;

/**
 * The house models to suggest: none once the user has dismissed the suggestion or has any model,
 * and none when the catalogue is empty.
 */
export function houseModelSuggestion(
  inventory: ModelInventory,
  dismissed: boolean,
  catalogue: readonly HouseModel[] = HOUSE_MODELS,
): HouseModel[] {
  if (dismissed || hasAnyModel(inventory)) return [];
  return [...catalogue];
}

/** Whether a model can be downloaded today (it has a source). */
export const downloadable = (model: HouseModel): model is HouseModel & { source: HouseModelSource } => model.source !== null;
