// The settings of the model configuration dialog, as one declarative table: for each setting its
// label, the group it sits in, what it affects, and the plain-words explanation behind its (i)
// icon. The dialog draws its rows from this table, so a setting's wording is edited in one place
// and a setting missing from it cannot be drawn (test/modelSettingsTable.test.ts checks that
// every field of the forms has an entry). Pure.

import type { LocalDraft, TableDraft } from "./modelConfigDrafts.ts";

/** What a setting changes, shown as a small tag beside its explanation. */
export type Aspect = "availability" | "memory" | "speed" | "quality" | "routing" | "cost" | "naming" | "reliability";

export const ASPECT_LABELS: Record<Aspect, string> = {
  availability: "availability",
  memory: "memory",
  speed: "speed",
  quality: "quality",
  routing: "routing",
  cost: "cost",
  naming: "names",
  reliability: "reliability",
};

export type SettingSpec<K extends string> = {
  key: K;
  label: string;
  /** The group (a table) the setting is drawn in. */
  group: string;
  /** What the setting affects, most important first. */
  affects: readonly Aspect[];
  /** Plain words: what the setting does and what changing it trades off. */
  help: string;
  /** Only llama.cpp models have it: shown but not editable for a vLLM model. */
  llamacppOnly?: boolean;
};

export type SettingGroup = {
  id: string;
  title: string;
  /** One line under the title, when the group needs it. */
  note?: string;
};

/** The settings that are not form fields: the on/off switch, which the daemon keeps apart. */
export type LocalKey = keyof LocalDraft | "enabled";
export type TableKey = keyof TableDraft;

export const LOCAL_GROUPS: readonly SettingGroup[] = [
  { id: "identity", title: "Identity and availability" },
  { id: "memory", title: "Context and memory", note: "Bigger context and a fuller KV pool take more memory; the estimate above follows your changes." },
  { id: "speed", title: "Speed: speculative decoding", note: "A small helper model drafts text that this one only has to check." },
  { id: "limits", title: "Limits" },
  { id: "advanced", title: "Advanced" },
];

export const TABLE_GROUPS: readonly SettingGroup[] = [
  { id: "identity", title: "Identity and availability" },
  { id: "routing", title: "Routing", note: "How routing and agents decide when to use this model." },
  { id: "cost", title: "Context and cost" },
];

export const LOCAL_SETTINGS: readonly SettingSpec<LocalKey>[] = [
  {
    key: "enabled",
    label: "Use this model",
    group: "identity",
    affects: ["availability"],
    help: "Turns the model on or off everywhere. Off, it is greyed out here and in the lists, left out of model pickers, never chosen by routing or by a retry on a stronger model, never offered to agents or the API, and it cannot be the default model. Your settings for it are kept, and you can turn it on again at any time.",
  },
  {
    key: "alias",
    label: "Display name",
    group: "identity",
    affects: ["naming"],
    help: "A friendlier name shown instead of the model's file name wherever you pick or see it. Routing, config files and --model still use the original id.",
  },
  {
    key: "card",
    label: "Model card",
    group: "identity",
    affects: ["routing"],
    help: "Notes on what this model is good at. Agents read the card, with what the model's own file says, when they choose a model to hand work to, so a clear note sends the right jobs here.",
  },
  {
    key: "context",
    label: "Max context per agent (tokens)",
    group: "memory",
    affects: ["memory", "quality"],
    help: "How much text one agent can keep in view at once, in tokens (about three quarters of a word each). More lets agents work on bigger files and longer chats, but the memory that holds it (the KV cache) grows with it. A number sets it; auto uses what the model was trained for, cut down to what fits in memory.",
  },
  {
    key: "pool_tokens",
    label: "KV pool (tokens)",
    group: "memory",
    affects: ["memory", "speed"],
    help: "The memory reserved up front for every slot's conversation together. Empty starts small and grows as agents need it, which saves memory but can pause a call while it grows. A number reserves that much at once: more memory in use from the start, steadier speed. It must be at least the max context.",
  },
  {
    key: "slots",
    label: "Slots",
    group: "memory",
    affects: ["speed", "memory"],
    help: "How many calls the model serves at the same time, from 1 to 256. More slots let several agents work in parallel instead of queueing, but they share the KV pool, so each gets less room unless the pool is larger.",
  },
  {
    key: "cache_type_k",
    label: "K cache type",
    group: "memory",
    affects: ["memory", "quality"],
    help: "How precisely the cache stores the \"keys\" the model looks up (attention keys). A lower-precision type such as q8_0 or q4_0 makes the cache much smaller (q8_0 is about half of f16) at a small cost in answer quality. f16 is the default.",
    llamacppOnly: true,
  },
  {
    key: "cache_type_v",
    label: "V cache type",
    group: "memory",
    affects: ["memory", "quality"],
    help: "How precisely the cache stores the \"values\" it reads back. A lower-precision type shrinks the cache like the K setting, usually with a bit more quality loss, and needs flash attention to be on or auto.",
    llamacppOnly: true,
  },
  {
    key: "flash_attn",
    label: "Flash attention",
    group: "memory",
    affects: ["speed", "memory"],
    help: "A faster, leaner way to compute attention. Auto turns it on when the graphics card supports it. It saves memory on long contexts and is required for a quantised V cache; turn it off only to work around a driver problem.",
    llamacppOnly: true,
  },
  {
    key: "max_gpu_layers",
    label: "Max GPU layers",
    group: "memory",
    affects: ["memory", "speed"],
    help: "The most of the model's layers to place on the graphics card. Each layer on the GPU is faster and uses VRAM; the rest run on the CPU from system RAM, which is much slower. Empty lets the planner put as many as fit; it may place fewer when memory is short.",
    llamacppOnly: true,
  },
  {
    key: "draft_model",
    label: "Draft model",
    group: "speed",
    affects: ["speed", "memory"],
    help: "Speculative decoding: a smaller installed model with the same tokenizer drafts a few words ahead and this model only checks them, which speeds up generation without changing the answer. The draft takes extra memory. Empty means none; if it is missing, incompatible or does not fit, the model runs without it.",
    llamacppOnly: true,
  },
  {
    key: "draft_max",
    label: "Draft max tokens",
    group: "speed",
    affects: ["speed"],
    help: "The most tokens the draft model proposes per step. Longer drafts help when they are usually right and waste work when they are not. Empty uses llama.cpp's default.",
    llamacppOnly: true,
  },
  {
    key: "draft_min",
    label: "Draft min tokens",
    group: "speed",
    affects: ["speed"],
    help: "The fewest tokens the draft model proposes per step (0 is allowed). Empty uses llama.cpp's default of 0.",
    llamacppOnly: true,
  },
  {
    key: "draft_gpu_layers",
    label: "Draft GPU layers",
    group: "speed",
    affects: ["memory", "speed"],
    help: "How many of the draft model's layers go on the graphics card; 0 keeps it in system RAM (slower, but frees VRAM). Empty lets llama.cpp place them, normally all.",
    llamacppOnly: true,
  },
  {
    key: "max_wait_s",
    label: "Max wait (s)",
    group: "limits",
    affects: ["reliability"],
    help: "The longest a call waits for a free slot, or for room to load the model, before giving up with an error. Raise it on a busy or small machine so queued work is not dropped.",
  },
  {
    key: "max_output_tokens",
    label: "Max output tokens",
    group: "limits",
    affects: ["memory", "reliability"],
    help: "The most tokens one reply may generate before it is retried without a cap. A cap keeps a model that rambles from filling its context; 0 means no cap, which reserves the whole context for the reply.",
  },
  {
    key: "extra_args",
    label: "Extra arguments (one per line)",
    group: "advanced",
    affects: ["reliability"],
    help: "Raw command-line flags added when this model's server starts, one per line, after the global ones. For tuning that has no setting here; a wrong flag can stop the model loading.",
  },
];

export const TABLE_SETTINGS: readonly SettingSpec<TableKey>[] = [
  {
    key: "enabled",
    label: "Use this model",
    group: "identity",
    affects: ["availability"],
    help: "Turns the row on or off everywhere. Off, it is greyed out here and in the lists, left out of model pickers, never chosen by routing or by a retry on a stronger model, and never offered to agents or the API. It cannot be the default model while it is off. Your settings are kept, and you can turn it back on at any time.",
  },
  {
    key: "alias",
    label: "Display name",
    group: "identity",
    affects: ["naming"],
    help: "A friendlier name shown instead of the row's id wherever you pick or see it. Routing rules, config files and --model still use the id.",
  },
  {
    key: "card",
    label: "Model card",
    group: "identity",
    affects: ["routing"],
    help: "Notes on what this model is good at. Agents read the card when they choose a model to hand work to, so a clear note sends the right jobs here.",
  },
  {
    key: "tier",
    label: "Tier",
    group: "routing",
    affects: ["routing", "cost"],
    help: "How capable routing treats the model: basic, standard, strong or frontier. A task that needs a higher tier skips lower ones, and a failed task is retried on the tier above, so a higher tier means the model gets harder (and usually costlier) work.",
  },
  {
    key: "tags",
    label: "Tags (comma-separated)",
    group: "routing",
    affects: ["routing"],
    help: "Labels that routing and your rules match against, such as vision (reads images), coder, or no-tools (cannot call tools). A task is only sent to a model that has the tags it needs.",
  },
  {
    key: "context_window",
    label: "Context window (tokens)",
    group: "cost",
    affects: ["routing", "quality"],
    help: "How much text the model accepts at once, in tokens. Routing skips a model whose window is too small for the task, and long chats are summarised to fit it. Empty means unknown.",
  },
  {
    key: "input_usd_per_mtok",
    label: "Input price ($/Mtok)",
    group: "cost",
    affects: ["cost", "routing"],
    help: "What the provider charges per million tokens you send. It drives budgets and the choice of the cheapest route. Set both prices or neither; empty means free or unknown.",
  },
  {
    key: "output_usd_per_mtok",
    label: "Output price ($/Mtok)",
    group: "cost",
    affects: ["cost", "routing"],
    help: "What the provider charges per million tokens the model writes. It drives budgets and the choice of the cheapest route. Set both prices or neither.",
  },
];

/** The explanations of the memory estimate's rows, in the order they are drawn. */
export const ESTIMATE_ROWS = [
  {
    key: "vram",
    label: "VRAM (graphics card memory)",
    help: "Graphics card memory the model would take with these settings: the layers placed on the GPU plus the KV cache. Past what the card has free, layers move to system RAM and the model slows down.",
  },
  {
    key: "ram",
    label: "RAM (system memory)",
    help: "System memory the model would take: the layers that do not fit on the graphics card (all of them on the CPU lane or without a GPU) and anything else the runtime keeps there.",
  },
  {
    key: "total",
    label: "Total needed",
    help: "Everything the model needs to load: weights, KV pool, working buffers and a draft model if one is used. It is spread over VRAM and RAM as the planner decides.",
  },
  {
    key: "weights",
    label: "Model weights",
    help: "The size of the model file itself. It depends on the model and its quantisation (a Q4 file is about a quarter of the f16 size) and does not change with these settings.",
  },
  {
    key: "kv",
    label: "KV cache (conversation memory)",
    help: "The memory that holds the conversations the model is working on. It grows with the KV pool and the cache types, so lowering the context, the pool, or the cache precision is the quickest way to cut memory.",
  },
  {
    key: "placement",
    label: "Where it runs",
    help: "How the model would load on this computer if nothing else were loaded: entirely on the graphics card, split with the CPU (slower), on the CPU only, or too large to load.",
  },
] as const;

export type EstimateKey = (typeof ESTIMATE_ROWS)[number]["key"];

/** The settings of a group, in table order; for a vLLM model the llama.cpp-only ones stay listed. */
export function settingsOf<K extends string>(settings: readonly SettingSpec<K>[], group: string): SettingSpec<K>[] {
  return settings.filter((setting) => setting.group === group);
}

/** The groups that have settings, each with its settings, in table order. */
export function groupedSettings<K extends string>(
  groups: readonly SettingGroup[],
  settings: readonly SettingSpec<K>[],
): { group: SettingGroup; settings: SettingSpec<K>[] }[] {
  return groups
    .map((group) => ({ group, settings: settingsOf(settings, group.id) }))
    .filter((entry) => entry.settings.length > 0);
}

/** The setting for `key`; throws for a key the table does not know, which is a bug. */
export function settingFor<K extends string>(settings: readonly SettingSpec<K>[], key: K): SettingSpec<K> {
  const found = settings.find((setting) => setting.key === key);
  if (!found) throw new Error(`no setting ${key} in the table`);
  return found;
}

/** The affected aspects as words, for a screen reader and the tooltip: "Affects memory and quality." */
export function affectsText(affects: readonly Aspect[]): string {
  const words = affects.map((aspect) => ASPECT_LABELS[aspect]);
  if (words.length === 0) return "";
  const list = words.length === 1 ? words[0] : `${words.slice(0, -1).join(", ")} and ${words[words.length - 1]}`;
  return `Affects ${list}.`;
}
