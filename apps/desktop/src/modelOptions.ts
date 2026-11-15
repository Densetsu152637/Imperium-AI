// The models a session can choose from, as list options (`models.list` rows and `models.local`
// models): what each is described as, which are left out, and how the session's own model stays
// shown when it is not among them. Pure (test/modelOptions.test.ts); `panels/ModelSelect.tsx`
// draws the lists.

import type { LocalModelRow, ModelRow } from "@imperium/protocol";

import type { SelectOption } from "./components/Select";
import { rowById, rowBySpecOrId } from "./models/spec.ts";
import { chooserModels, type ModelGroup, type ModelMarks } from "./modelPrefs.ts";

const RUNTIMES: Record<LocalModelRow["runtime"], string> = { llamacpp: "llama.cpp", vllm: "vLLM" };

/** The spec a local model is used by: `<runtime>:<id>`. */
export const localSpec = (row: LocalModelRow): string => `${row.runtime}:${row.id}`;

/** Tokens in words: 32768 → "32k", 1048576 → "1M". */
export function tokensShort(tokens: number): string {
  if (tokens >= 1024 * 1024) return `${+(tokens / (1024 * 1024)).toFixed(1)}M`;
  if (tokens >= 1024) return `${Math.round(tokens / 1024)}k`;
  return String(tokens);
}

/** Bytes on disk in words: "2.3 GB", "640 MB". */
export function sizeShort(bytes: number): string {
  const gb = bytes / 1024 ** 3;
  return gb >= 1 ? `${gb.toFixed(1)} GB` : `${Math.round(bytes / 1024 ** 2)} MB`;
}

/**
 * Where a local model runs, from the daemon's plan for it under its settings (live values while
 * it is loaded): "36/36 layers GPU · 3.1 GB VRAM + 0.4 GB RAM", "22/48 layers GPU · 9.8 GB VRAM +
 * 11.2 GB RAM" when it would split, "CPU · 6.2 GB RAM" on the CPU lane, "too large" when it does
 * not fit this machine.
 */
export function memoryPlan(row: LocalModelRow): string {
  if (row.problem) return "cannot run here";
  if (!row.loaded && row.placement === "none") return "too large";
  const memory = [row.vram_bytes > 0 ? `${sizeShort(row.vram_bytes)} VRAM` : null, row.ram_bytes > 0 ? `${sizeShort(row.ram_bytes)} RAM` : null]
    .filter(Boolean)
    .join(" + ");
  const onGpu = row.gpu_layers ?? 0;
  if (row.cpu_lane || (onGpu === 0 && row.vram_bytes === 0)) return ["CPU", memory].filter(Boolean).join(" · ");
  const layers = row.layers != null ? `${Math.min(onGpu, row.layers)}/${row.layers} layers GPU` : "GPU";
  return [layers, memory].filter(Boolean).join(" · ");
}

/**
 * A local model in a line: quantisation, context (the trained length, the most one agent may
 * use, and the KV cache reserved, now while loaded or at first, when it is less), the daemon's
 * memory plan for it ([`memoryPlan`]), and the runtime — such as "Q4_K_M · 256k ctx · up to
 * 128k/agent (auto) · 16k reserved · 36/36 layers GPU · 3.1 GB VRAM · loaded · llama.cpp".
 */
export function localDescription(row: LocalModelRow): string {
  const parts: string[] = [];
  parts.push(row.quantization ?? (row.runtime === "vllm" ? "safetensors" : "?"));
  if (row.context_length) parts.push(`${tokensShort(row.context_length)} ctx`);
  if (row.context > 0) {
    parts.push(`up to ${tokensShort(row.context)}/agent${row.context_auto ? " (auto)" : ""}`);
    if (row.runtime === "llamacpp" && row.pool_tokens > 0 && row.pool_tokens < row.context) {
      parts.push(`${tokensShort(row.pool_tokens)} reserved`);
    }
  }
  parts.push(memoryPlan(row));
  if (row.loaded) parts.push("loaded");
  parts.push(RUNTIMES[row.runtime]);
  return parts.join(" · ");
}

/**
 * The models a session can use, by spec: table rows first (rows whose endpoint is not configured
 * are listed but cannot be chosen), then local models the table does not already list. With
 * `marks` ([`chooserModels`]), favourites come first under "Favourites" and hidden models are
 * left out unless one is `current`. A `current` model outside both is listed as is, so the list
 * always shows it. A `current` model that is switched off is kept (marked off) only with
 * `liveSession`, the picker of an open session; a new-session picker leaves it out, so a model
 * that is off cannot be chosen.
 */
export function modelOptions(
  models: ModelRow[],
  local: LocalModelRow[],
  current: string | null,
  marks?: ModelMarks,
  liveSession = false,
): SelectOption[] {
  const all = allModelOptions(models, local);
  const chosen = chooserModels(all, (option) => option.value, marks, current);
  const grouped = chosen.some((entry) => entry.group !== "normal");
  const options: SelectOption[] = chosen.map(({ item, group }) =>
    grouped ? { ...item, group: GROUP_LABELS[group], dimmed: group === "hidden" } : item,
  );
  if (current && !options.some((option) => option.value === current)) {
    const entry = currentOption(models, local, current, liveSession);
    if (entry) options.unshift(entry);
  }
  return options;
}

const GROUP_LABELS: Record<ModelGroup, string> = { favourite: "Favourites", normal: "Models", hidden: "Hidden" };

/** What the list calls a model that is switched off. */
export const OFF_LABEL = "off";

/**
 * The entry for a session's `current` model when the list does not offer it: a model switched
 * off ("Use this model") after the session started is shown by its name with "off" beside it
 * and dimmed, not as a bare spec; a model the settings do not know at all is listed as it is.
 */
export function currentOption(
  models: ModelRow[],
  local: LocalModelRow[],
  current: string,
  liveSession = true,
): SelectOption | null {
  // The session's model is a spec or a model table id.
  const row = rowBySpecOrId(models, current);
  const localRow = local.find((candidate) => localSpec(candidate) === current);
  const off = (row != null && !row.enabled) || (localRow != null && !localRow.enabled);
  if (!off) return { value: current, label: current };
  // Off: shown for the open session that is on it, never offered to a new one.
  if (!liveSession) return null;
  const name = row?.alias ?? localRow?.alias ?? row?.id ?? localRow?.id ?? current;
  return {
    value: current,
    label: `${name} (${OFF_LABEL})`,
    description: "Switched off in the model settings: this session keeps it until another model is chosen",
    dimmed: true,
  };
}

/** Every model a session could use, in the table's order, before favourites and hidden models apply. */
function allModelOptions(models: ModelRow[], local: LocalModelRow[]): SelectOption[] {
  // A model switched off in the model settings ("Use this model") is not offered.
  const options: SelectOption[] = models.filter((row) => row.enabled).map((row) => {
    // A table row that names a local model also says what that model is.
    const localRow = local.find((candidate) => localSpec(candidate) === row.spec);
    // Shown by its alias (or the local model's it names), with the id kept in the description.
    const alias = row.alias ?? localRow?.alias ?? null;
    return {
      value: row.spec,
      label: alias ?? row.id,
      disabled: !row.endpoint_configured,
      description: row.endpoint_configured
        ? [alias ? row.id : null, row.id === row.spec ? null : row.spec, localRow ? localDescription(localRow) : row.location, row.tier]
            .filter(Boolean)
            .join(" · ")
        : [alias ? row.id : null, "Endpoint not configured"].filter(Boolean).join(" · "),
    };
  });
  for (const row of local) {
    const spec = localSpec(row);
    if (!row.enabled || options.some((option) => option.value === spec)) continue;
    options.push({
      value: spec,
      label: row.alias ?? row.id,
      description: row.alias ? `${row.id} · ${localDescription(row)}` : localDescription(row),
      disabled: row.problem != null,
    });
  }
  return options;
}

/** A configured model name (a table id or a spec) as the spec the list uses. */
export function asSpec(models: ModelRow[], name: string | null): string | null {
  if (!name) return null;
  return rowById(models, name)?.spec ?? name;
}
