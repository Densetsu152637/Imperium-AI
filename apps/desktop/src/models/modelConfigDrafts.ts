// The model configuration form's data: the drafts (what the fields hold as typed, an empty string
// meaning "not set"), their conversion to the settings the daemon saves, and the client-side
// validation messages (the daemon is authoritative). Pure, so the rules are tested without a
// window (test/modelConfigDrafts.test.ts); ModelConfigDialog draws the form.

import type {
  LocalLaunchSettings,
  LocalModelConfig,
  TableModelSettings,
} from "@imperium/protocol";
import { checkDraft } from "./draftSettings.ts";

export const TIERS = ["basic", "standard", "strong", "frontier"] as const;
export const FLASH = ["auto", "on", "off"] as const;
const FULL_PRECISION_V = ["f16", "bf16", "f32"];

/** Local form fields as typed: an empty string means "not set" (sent as null). */
export type LocalDraft = {
  alias: string;
  card: string;
  context: string;
  pool_tokens: string;
  slots: string;
  cache_type_k: string;
  cache_type_v: string;
  flash_attn: string;
  max_gpu_layers: string;
  max_wait_s: string;
  max_output_tokens: string;
  extra_args: string;
  draft_model: string;
  draft_max: string;
  draft_min: string;
  draft_gpu_layers: string;
};

export type TableDraft = {
  alias: string;
  card: string;
  enabled: boolean | null;
  tier: string;
  tags: string;
  context_window: string;
  input_usd_per_mtok: string;
  output_usd_per_mtok: string;
};

const num = (value: number | null): string => (value === null ? "" : String(value));

export function localDraft(saved: LocalLaunchSettings): LocalDraft {
  return {
    alias: saved.alias ?? "",
    card: saved.card ?? "",
    context: saved.context_auto ? "auto" : num(saved.context),
    pool_tokens: num(saved.pool_tokens),
    slots: num(saved.slots),
    cache_type_k: saved.cache_type_k ?? "",
    cache_type_v: saved.cache_type_v ?? "",
    flash_attn: saved.flash_attn ?? "",
    max_gpu_layers: num(saved.max_gpu_layers),
    max_wait_s: num(saved.max_wait_s),
    max_output_tokens: num(saved.max_output_tokens),
    extra_args: (saved.extra_args ?? []).join("\n"),
    draft_model: saved.draft_model ?? "",
    draft_max: num(saved.draft_max),
    draft_min: num(saved.draft_min),
    draft_gpu_layers: num(saved.draft_gpu_layers),
  };
}

export function tableDraft(saved: TableModelSettings): TableDraft {
  return {
    alias: saved.alias ?? "",
    card: saved.card ?? "",
    enabled: saved.enabled,
    tier: saved.tier ?? "",
    tags: (saved.tags ?? []).join(", "),
    context_window: num(saved.context_window),
    input_usd_per_mtok: num(saved.input_usd_per_mtok),
    output_usd_per_mtok: num(saved.output_usd_per_mtok),
  };
}

export const EMPTY_LOCAL: LocalDraft = localDraft({
  alias: null,
  card: null,
  context: null,
  context_auto: false,
  pool_tokens: null,
  slots: null,
  cache_type_k: null,
  cache_type_v: null,
  flash_attn: null,
  max_gpu_layers: null,
  max_wait_s: null,
  max_output_tokens: null,
  extra_args: null,
  draft_model: null,
  draft_max: null,
  draft_min: null,
  draft_gpu_layers: null,
});

export const EMPTY_TABLE: TableDraft = tableDraft({
  alias: null,
  card: null,
  enabled: null,
  tier: null,
  tags: null,
  context_window: null,
  input_usd_per_mtok: null,
  output_usd_per_mtok: null,
});

/** Longest alias the daemon accepts, in characters (imperium-config's MAX_ALIAS_CHARS). */
export const MAX_ALIAS_CHARS = 80;

/** An alias from its field: trimmed, null when empty; throws a message when too long. */
export function aliasValue(text: string): string | null {
  const trimmed = text.trim().replace(/\s+/g, " ");
  if (trimmed === "") return null;
  if ([...trimmed].length > MAX_ALIAS_CHARS) throw new Error(`Display name must be at most ${MAX_ALIAS_CHARS} characters`);
  return trimmed;
}

/** Longest model card the daemon accepts, in characters (imperium-config's MAX_CARD_CHARS). */
export const MAX_CARD_CHARS = 4000;

/** A model card from its field: trimmed, null when empty; throws a message when too long. */
export function cardValue(text: string): string | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  if ([...trimmed].length > MAX_CARD_CHARS) throw new Error(`The model card must be at most ${MAX_CARD_CHARS} characters`);
  return trimmed;
}

/** A whole number from a field, or null when empty; throws a message for anything else. */
function integer(text: string, label: string, min: number): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  if (!/^\d+$/.test(trimmed)) throw new Error(`${label} must be a whole number`);
  const value = Number.parseInt(trimmed, 10);
  if (value < min) throw new Error(`${label} must be at least ${min}`);
  return value;
}

function price(text: string, label: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  if (!Number.isFinite(value) || value < 0) throw new Error(`${label} must be a number, 0 or more`);
  return value;
}

/** Form to settings; throws the first client-side validation message (the server is authoritative). */
export function localSettings(draft: LocalDraft, config: LocalModelConfig): LocalLaunchSettings {
  const contextAuto = draft.context.trim().toLowerCase() === "auto";
  const context = contextAuto ? null : integer(draft.context, "Max context (a number or auto)", 1);
  const pool = integer(draft.pool_tokens, "KV pool", 1);
  const slots = integer(draft.slots, "Slots", 1);
  if (slots !== null && slots > 256) throw new Error("Slots must be at most 256");
  const args = draft.extra_args
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  const vllm = config.runtime === "vllm";
  const settings: LocalLaunchSettings = {
    alias: aliasValue(draft.alias),
    card: cardValue(draft.card),
    context,
    context_auto: contextAuto,
    pool_tokens: pool,
    slots,
    cache_type_k: vllm ? null : draft.cache_type_k || null,
    cache_type_v: vllm ? null : draft.cache_type_v || null,
    flash_attn: vllm ? null : ((draft.flash_attn || null) as LocalLaunchSettings["flash_attn"]),
    max_gpu_layers: vllm ? null : integer(draft.max_gpu_layers, "Max GPU layers", 0),
    max_wait_s: integer(draft.max_wait_s, "Max wait", 1),
    max_output_tokens: integer(draft.max_output_tokens, "Max output tokens", 0),
    extra_args: args.length > 0 ? args : null,
    draft_model: vllm ? null : draft.draft_model.trim() || null,
    draft_max: vllm ? null : integer(draft.draft_max, "Draft max tokens", 1),
    draft_min: vllm ? null : integer(draft.draft_min, "Draft min tokens", 0),
    draft_gpu_layers: vllm ? null : integer(draft.draft_gpu_layers, "Draft GPU layers", 0),
  };
  checkDraft(config.id, {
    model: settings.draft_model,
    max: settings.draft_max,
    min: settings.draft_min,
    gpuLayers: settings.draft_gpu_layers,
  }, config.effective.draft_max);
  // A fixed pool must hold a fixed context; an `auto` context is capped to the pool instead.
  const fixedContext = contextAuto ? null : (context ?? (config.effective.context_auto ? null : config.effective.context));
  if (pool !== null && fixedContext !== null && pool < fixedContext) {
    throw new Error("A fixed KV pool must be at least the max context");
  }
  const v = settings.cache_type_v ?? config.effective.cache_type_v;
  const flash = settings.flash_attn ?? config.effective.flash_attn;
  if (!vllm && !FULL_PRECISION_V.includes(v) && flash === "off") {
    throw new Error("A quantised V cache type needs flash attention on or auto");
  }
  return settings;
}

export function tableSettings(draft: TableDraft): TableModelSettings {
  const input = price(draft.input_usd_per_mtok, "Input price");
  const output = price(draft.output_usd_per_mtok, "Output price");
  if ((input === null) !== (output === null)) throw new Error("Set both prices or neither");
  const tags = draft.tags
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean);
  return {
    alias: aliasValue(draft.alias),
    card: cardValue(draft.card),
    enabled: draft.enabled,
    tier: (draft.tier || null) as TableModelSettings["tier"],
    tags: tags.length > 0 ? tags : null,
    context_window: integer(draft.context_window, "Context window", 1),
    input_usd_per_mtok: input,
    output_usd_per_mtok: output,
  };
}

export const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);

/** Normalises settings the way the form round-trips them, so "unchanged" compares equal. */
export const normLocal = (s: LocalLaunchSettings): LocalLaunchSettings => ({
  ...s,
  extra_args: s.extra_args && s.extra_args.length > 0 ? s.extra_args : null,
});
export const normTable = (s: TableModelSettings): TableModelSettings => ({
  ...s,
  tags: s.tags && s.tags.length > 0 ? s.tags : null,
});
