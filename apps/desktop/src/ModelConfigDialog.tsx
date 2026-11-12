// Model configuration: llama.cpp / vLLM settings of local models and the editable settings of
// model-table rows. Edits are saved through `models.set_config`; a debounced dry run supplies the
// live memory estimate and the server's validation message. It uses the Settings window's shell
// (SettingsModal): the models are its navigation, local ones and model-table rows in two groups.

import { Boxes, Cpu, Search } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import type {
  LocalLaunchSettings,
  LocalModelConfig,
  ModelConfigResult,
  ModelSetConfigParams,
  TableModelConfig,
  TableModelSettings,
} from "@imperium/protocol";
import { matches, ModalNavGroup, ModalNavItem, SettingsModal } from "./components/SettingsModal";
import type { DaemonClient } from "./daemon/client";
import { arrangeModels, markOf } from "./modelPrefs";
import { useSettings } from "./settings";

export type ModelDialogTarget = { tab: "local" | "table"; id?: string };

const GIB = 1024 ** 3;
const gib = (bytes: number): string => `${(bytes / GIB).toFixed(1)} GiB`;
const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));
const TIERS = ["basic", "standard", "strong", "frontier"] as const;
const FLASH = ["auto", "on", "off"] as const;
const FULL_PRECISION_V = ["f16", "bf16", "f32"];

/** Local form fields as typed: an empty string means "not set" (sent as null). */
type LocalDraft = {
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

type TableDraft = {
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

function localDraft(saved: LocalLaunchSettings): LocalDraft {
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

function tableDraft(saved: TableModelSettings): TableDraft {
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

const EMPTY_LOCAL: LocalDraft = localDraft({
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

const EMPTY_TABLE: TableDraft = tableDraft({
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
const MAX_ALIAS_CHARS = 80;

/** An alias from its field: trimmed, null when empty; throws a message when too long. */
export function aliasValue(text: string): string | null {
  const trimmed = text.trim().replace(/\s+/g, " ");
  if (trimmed === "") return null;
  if ([...trimmed].length > MAX_ALIAS_CHARS) throw new Error(`Display name must be at most ${MAX_ALIAS_CHARS} characters`);
  return trimmed;
}

/** Longest model card the daemon accepts, in characters (imperium-config's MAX_CARD_CHARS). */
const MAX_CARD_CHARS = 4000;

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
function localSettings(draft: LocalDraft, config: LocalModelConfig): LocalLaunchSettings {
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
  if (settings.draft_model === null && (settings.draft_max !== null || settings.draft_min !== null || settings.draft_gpu_layers !== null)) {
    throw new Error("The draft settings need a draft model");
  }
  if (settings.draft_model === config.id) throw new Error("A model cannot draft for itself");
  if (settings.draft_max !== null && settings.draft_min !== null && settings.draft_min > settings.draft_max) {
    throw new Error("Draft min tokens may not exceed draft max tokens");
  }
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

function tableSettings(draft: TableDraft): TableModelSettings {
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

const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);

/** Normalises settings the way the form round-trips them, so "unchanged" compares equal. */
const normLocal = (s: LocalLaunchSettings): LocalLaunchSettings => ({
  ...s,
  extra_args: s.extra_args && s.extra_args.length > 0 ? s.extra_args : null,
});
const normTable = (s: TableModelSettings): TableModelSettings => ({
  ...s,
  tags: s.tags && s.tags.length > 0 ? s.tags : null,
});

function Field({
  label,
  hint,
  filled,
  onReset,
  children,
}: {
  label: string;
  hint?: string;
  filled: boolean;
  onReset: () => void;
  children: ReactNode;
}) {
  return (
    <label className="setting model-field">
      <span>
        {label}
        {filled && (
          <button type="button" className="ghost reset" onClick={onReset} title="Use the default">
            reset
          </button>
        )}
      </span>
      {children}
      {hint && <small className="muted">{hint}</small>}
    </label>
  );
}

export function ModelConfigDialog({
  client,
  target,
  onClose,
  onSaved,
}: {
  client: DaemonClient | null;
  target: ModelDialogTarget;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { modelMarks } = useSettings();
  const [config, setConfig] = useState<ModelConfigResult | null>(null);
  const [tab, setTab] = useState(target.tab);
  const [selected, setSelected] = useState<string | null>(target.id ?? null);
  const [query, setQuery] = useState("");
  const [localDrafts, setLocalDrafts] = useState<Record<string, LocalDraft>>({});
  const [tableDrafts, setTableDrafts] = useState<Record<string, TableDraft>>({});
  const [preview, setPreview] = useState<LocalModelConfig | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [applied, setApplied] = useState(false);

  useEffect(() => {
    if (!client) return;
    client.call<ModelConfigResult>("models.config").then(setConfig, (e) => setError(message(e)));
  }, [client]);

  const localById = useMemo(() => new Map((config?.local ?? []).map((m) => [m.id, m])), [config]);
  const tableById = useMemo(() => new Map((config?.table ?? []).map((m) => [m.id, m])), [config]);

  // Keep a valid selection: the asked-for kind first, else the other when that has no models.
  useEffect(() => {
    if (!config) return;
    const ids = (tab === "local" ? config.local : config.table).map((m) => m.id);
    if (selected && ids.includes(selected)) return;
    const other = tab === "local" ? config.table : config.local;
    if (ids.length === 0 && other.length > 0) {
      setTab(tab === "local" ? "table" : "local");
      setSelected(other[0].id);
    } else {
      setSelected(ids[0] ?? null);
    }
  }, [config, tab, selected]);

  const selectedLocal = tab === "local" && selected ? localById.get(selected) : undefined;
  const selectedTable = tab === "table" && selected ? tableById.get(selected) : undefined;
  const selectedAlias = selectedLocal?.saved.alias ?? selectedTable?.effective.alias ?? null;

  /** One model's settings if its form differs from what is saved; throws a validation message. */
  const localChange = (model: LocalModelConfig): LocalLaunchSettings | null => {
    const draft = localDrafts[model.id];
    if (!draft) return null;
    const settings = localSettings(draft, model);
    return same(normLocal(settings), normLocal(model.saved)) ? null : settings;
  };
  const tableChange = (row: TableModelConfig): TableModelSettings | null => {
    const draft = tableDrafts[row.id];
    if (!draft) return null;
    const settings = tableSettings(draft);
    return same(normTable(settings), normTable(row.saved)) ? null : settings;
  };

  const collect = (): ModelSetConfigParams => {
    const params: ModelSetConfigParams = { local: [], table: [], hidden: null, dry_run: false };
    for (const model of config?.local ?? []) {
      const settings = localChange(model);
      if (settings) params.local.push({ id: model.id, settings });
    }
    for (const row of config?.table ?? []) {
      const settings = tableChange(row);
      if (settings) params.table.push({ id: row.id, settings });
    }
    return params;
  };

  // Debounced dry run of the selected model: live estimate and the server's validation message.
  const selectedLocalDraft = selectedLocal ? localDrafts[selectedLocal.id] : undefined;
  const selectedTableDraft = selectedTable ? tableDrafts[selectedTable.id] : undefined;
  useEffect(() => {
    setPreview(null);
    setPreviewError(null);
    if (!client) return;
    const params: ModelSetConfigParams = { local: [], table: [], hidden: null, dry_run: true };
    try {
      if (selectedLocal && selectedLocalDraft) {
        params.local.push({ id: selectedLocal.id, settings: localSettings(selectedLocalDraft, selectedLocal) });
      } else if (selectedTable && selectedTableDraft) {
        params.table.push({ id: selectedTable.id, settings: tableSettings(selectedTableDraft) });
      } else {
        return;
      }
    } catch (e) {
      setPreviewError(message(e));
      return;
    }
    let live = true;
    const timer = setTimeout(() => {
      client.call<ModelConfigResult>("models.set_config", params).then(
        (result) => {
          if (live) setPreview(result.local.find((m) => m.id === selectedLocal?.id) ?? null);
        },
        (e) => {
          if (live) setPreviewError(message(e));
        },
      );
    }, 500);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [client, selectedLocal, selectedTable, selectedLocalDraft, selectedTableDraft]);

  /**
   * Saves the changed models. With `close`, closes the dialog after; without (Apply), keeps it
   * open, shows what is now in effect (such as a pool cut to fit or the layers on the GPU), and
   * says so briefly. Closing after an Apply keeps what was applied.
   */
  const save = async (close: boolean) => {
    if (!client || saving) return;
    setError(null);
    let params: ModelSetConfigParams;
    try {
      params = collect();
    } catch (e) {
      setError(message(e));
      return;
    }
    if (params.local.length === 0 && params.table.length === 0) {
      if (close) onClose();
      return;
    }
    setSaving(true);
    try {
      const result = await client.call<ModelConfigResult>("models.set_config", params);
      onSaved();
      if (close) {
        onClose();
        return;
      }
      setConfig(result);
      setLocalDrafts({});
      setTableDrafts({});
      setApplied(true);
    } catch (e) {
      setError(message(e));
    } finally {
      setSaving(false);
    }
  };

  // The "Saved" note after an Apply fades after a moment.
  useEffect(() => {
    if (!applied) return;
    const timer = setTimeout(() => setApplied(false), 2500);
    return () => clearTimeout(timer);
  }, [applied]);

  const changed = (id: string, kind: "local" | "table"): boolean => {
    try {
      if (kind === "local") {
        const model = localById.get(id);
        return model ? localChange(model) !== null : false;
      }
      const row = tableById.get(id);
      return row ? tableChange(row) !== null : false;
    } catch {
      return true;
    }
  };
  const dirty =
    config !== null &&
    (config.local.some((m) => changed(m.id, "local")) || config.table.some((r) => changed(r.id, "table")));

  const setLocal = (model: LocalModelConfig, patch: Partial<LocalDraft>) =>
    setLocalDrafts((all) => ({ ...all, [model.id]: { ...(all[model.id] ?? localDraft(model.saved)), ...patch } }));
  const setTable = (row: TableModelConfig, patch: Partial<TableDraft>) =>
    setTableDrafts((all) => ({ ...all, [row.id]: { ...(all[row.id] ?? tableDraft(row.saved)), ...patch } }));

  // Favourites first and hidden models last, as in the model panels (modelPrefs.ts).
  const specOf = (item: LocalModelConfig | TableModelConfig) => ("spec" in item ? item.spec : `${item.runtime}:${item.id}`);
  const aliasOf = (item: LocalModelConfig | TableModelConfig): string | null => ("spec" in item ? item.effective.alias : item.saved.alias);
  const arrange = <T extends LocalModelConfig | TableModelConfig>(list: T[]): T[] => {
    const arranged = arrangeModels<T>(
      list.filter((item) => matches(query, item.id, specOf(item), aliasOf(item) ?? "")),
      specOf,
      modelMarks,
    );
    return [...arranged.favourites, ...arranged.normal, ...arranged.hidden];
  };

  const navItem = (item: LocalModelConfig | TableModelConfig, kind: "local" | "table") => {
    const local = kind === "local" ? (item as LocalModelConfig) : undefined;
    const row = kind === "table" ? (item as TableModelConfig) : undefined;
    const mark = markOf(modelMarks, specOf(item));
    const alias = aliasOf(item);
    return (
      <ModalNavItem
        key={`${kind}:${item.id}`}
        icon={kind === "local" ? Cpu : Boxes}
        active={tab === kind && selected === item.id}
        dimmed={mark === "hidden"}
        title={alias ? `${alias} (${item.id})` : item.id}
        onClick={() => {
          setTab(kind);
          setSelected(item.id);
        }}
        badges={
          <>
            {mark === "favourite" && <span className="badge">favourite</span>}
            {mark === "hidden" && <span className="badge off">hidden</span>}
            {local && <span className="badge">{local.runtime}</span>}
            {row && <span className={`badge ${row.location}`}>{row.location}</span>}
            {local?.loaded && <span className="badge local">loaded</span>}
            {local?.reload_pending && <span className="badge">reload pending</span>}
            {changed(item.id, kind) && <span className="badge">edited</span>}
          </>
        }
      >
        {alias ? (
          <>
            {alias} <span className="muted">{item.id}</span>
          </>
        ) : (
          item.id
        )}
      </ModalNavItem>
    );
  };

  const localItems = arrange(config?.local ?? []);
  const tableItems = arrange(config?.table ?? []);
  const nav = (
    <>
      <label className="search-field settings-search">
        <Search aria-hidden="true" />
        <input
          type="search"
          placeholder="Search models"
          aria-label="Search models"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          autoFocus
        />
      </label>
      <ModalNavGroup label="Local models">
        {localItems.map((item) => navItem(item, "local"))}
        {config && localItems.length === 0 && (
          <span className="settings-nav-empty">{query.trim() ? "No matches." : "No local models found."}</span>
        )}
      </ModalNavGroup>
      <ModalNavGroup label="Model table">
        {tableItems.map((item) => navItem(item, "table"))}
        {config && tableItems.length === 0 && (
          <span className="settings-nav-empty">{query.trim() ? "No matches." : "The model table is empty."}</span>
        )}
      </ModalNavGroup>
    </>
  );

  const footer = (
    <>
      {error && (
        <span className="error" role="alert">
          {error}
        </span>
      )}
      {config && !error && <span className="muted">saved to {config.file}</span>}
      {applied && (
        <span className="muted" role="status">
          Saved
        </span>
      )}
      <button onClick={onClose}>{applied || !dirty ? "Close" : "Cancel"}</button>
      <button onClick={() => void save(false)} disabled={saving || !config || !dirty} title="Save without closing (Ctrl+S)">
        Apply
      </button>
      <button className="primary" onClick={() => void save(true)} disabled={saving || !config || !dirty}>
        {saving ? "Saving…" : "Save"}
      </button>
    </>
  );

  return (
    <SettingsModal
      label="Models"
      title={selected ? (selectedAlias ?? selected) : "Models"}
      nav={nav}
      footer={footer}
      onClose={onClose}
      onKeyDown={(event) => {
        // Ctrl+S (Cmd+S on macOS) applies without closing.
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
          event.preventDefault();
          if (dirty) void save(false);
        }
      }}
    >
      {!config && !error && <p className="muted">Loading…</p>}
      {config && !selectedLocal && !selectedTable && <p className="muted">No models to configure.</p>}
      {selectedLocal && config && (
        <LocalForm
          model={selectedLocal}
          draftChoices={config.local.filter((m) => m.runtime === "llamacpp" && m.id !== selectedLocal.id).map((m) => m.id)}
          draft={localDrafts[selectedLocal.id] ?? localDraft(selectedLocal.saved)}
          defaults={config.defaults}
          cacheTypes={config.cache_types}
          preview={preview}
          previewError={previewError}
          onChange={(patch) => setLocal(selectedLocal, patch)}
          onResetAll={() => setLocalDrafts((all) => ({ ...all, [selectedLocal.id]: EMPTY_LOCAL }))}
        />
      )}
      {selectedTable && (
        <TableForm
          row={selectedTable}
          draft={tableDrafts[selectedTable.id] ?? tableDraft(selectedTable.saved)}
          previewError={previewError}
          onChange={(patch) => setTable(selectedTable, patch)}
          onResetAll={() => setTableDrafts((all) => ({ ...all, [selectedTable.id]: EMPTY_TABLE }))}
        />
      )}
    </SettingsModal>
  );
}

/**
 * The model's display name: shown instead of its id wherever a model is named. The id stays what
 * config, routing and `--model` use. `fallback` is an alias the row has from config.toml.
 */
function AliasField({
  id,
  value,
  fallback,
  onChange,
}: {
  id: string;
  value: string;
  fallback: string | null;
  onChange: (alias: string) => void;
}) {
  return (
    <Field
      label="Display name"
      hint={`Shown instead of the id everywhere; the id (${id}) is unchanged in config and routing. Empty shows ${fallback ? "the configured alias" : "the id"}.`}
      filled={value !== ""}
      onReset={() => onChange("")}
    >
      <input type="text" value={value} placeholder={fallback ?? id} maxLength={MAX_ALIAS_CHARS} spellCheck={false} onChange={(e) => onChange(e.target.value)} />
    </Field>
  );
}

/**
 * The model card: what the model is good at (coding, writing, research, ...) and notes. Agents
 * read it, with what the model's metadata says, when they choose a model to delegate to.
 */
function CardField({
  value,
  metadata,
  fallback,
  onChange,
}: {
  value: string;
  metadata: string | null;
  fallback: string | null;
  onChange: (card: string) => void;
}) {
  return (
    <Field
      label="Model card"
      hint="What the model is good at and notes; agents read it when they pick a model to delegate to."
      filled={value !== ""}
      onReset={() => onChange("")}
    >
      <textarea
        rows={4}
        value={value}
        maxLength={MAX_CARD_CHARS}
        placeholder={fallback ?? "Good at: coding, tool use. Notes: fast; weak at long documents."}
        onChange={(e) => onChange(e.target.value)}
      />
      {metadata && (
        <details className="model-card-metadata">
          <summary>
            From the model's metadata{" "}
            {value === "" && (
              <button type="button" className="ghost reset" onClick={() => onChange(metadata)}>
                copy into the card
              </button>
            )}
          </summary>
          <pre className="muted">{metadata}</pre>
        </details>
      )}
    </Field>
  );
}

function LocalForm({
  model,
  draftChoices,
  draft,
  defaults,
  cacheTypes,
  preview,
  previewError,
  onChange,
  onResetAll,
}: {
  model: LocalModelConfig;
  /** Ids of the other installed llama.cpp models, the candidates for the draft. */
  draftChoices: string[];
  draft: LocalDraft;
  defaults: ModelConfigResult["defaults"];
  cacheTypes: string[];
  preview: LocalModelConfig | null;
  previewError: string | null;
  onChange: (patch: Partial<LocalDraft>) => void;
  onResetAll: () => void;
}) {
  const vllm = model.runtime === "vllm";
  const eff = model.effective;
  const estimate = preview ?? model;
  const text = (
    key: keyof LocalDraft,
    label: string,
    placeholder: number | null,
    options: { disabled?: boolean; hint?: string } = {},
  ) => (
    <Field label={label} hint={options.hint} filled={draft[key] !== ""} onReset={() => onChange({ [key]: "" })}>
      <input
        type="text"
        inputMode="numeric"
        value={draft[key]}
        placeholder={placeholder === null ? "planner decides" : String(placeholder)}
        disabled={options.disabled}
        onChange={(e) => onChange({ [key]: e.target.value })}
      />
    </Field>
  );
  const select = (
    key: "cache_type_k" | "cache_type_v" | "flash_attn",
    label: string,
    values: readonly string[],
    fallback: string,
  ) => (
    <Field label={label} filled={draft[key] !== ""} onReset={() => onChange({ [key]: "" })}>
      <select value={draft[key]} disabled={vllm} onChange={(e) => onChange({ [key]: e.target.value })}>
        <option value="">default ({fallback})</option>
        {values.map((value) => (
          <option key={value} value={value}>
            {value}
          </option>
        ))}
      </select>
    </Field>
  );
  return (
    <div className="model-form-fields">
      <div className="model-summary">
        <span className="muted">
          {model.saved.alias ? `${model.id} · ` : ""}
          {model.runtime}
          {model.cpu_lane ? " · CPU lane" : ""}
          {model.context_length ? ` · trained for ${model.context_length.toLocaleString()} tokens` : ""}
          {` · up to ${estimate.effective.context.toLocaleString()} per agent${estimate.effective.context_auto ? " (auto)" : ""}`}
        </span>
        <span>
          needs {gib(estimate.required_bytes)} · KV pool {gib(estimate.kv_pool_bytes)} · {estimate.placement}
        </span>
        {model.reload_pending && (
          <span className="warning">Loaded with other settings; the new ones apply once no session uses it.</span>
        )}
        {estimate.effective.pool_clamped && (
          <span className="warning">
            The KV pool does not fit beside the weights on the GPU; it is cut to {estimate.effective.pool_tokens.toLocaleString()} tokens so
            every layer stays on the GPU.
          </span>
        )}
        {estimate.draft_in_use && (
          <span className="muted">
            Drafting with {estimate.draft_in_use} (speculative decoding); its memory is in the figures above.
          </span>
        )}
        {estimate.draft_notice && <span className="warning">{estimate.draft_notice}</span>}
        {estimate.placement === "partial" && !vllm && (
          <span className="warning">The weights do not fit on the GPU: some layers run on the CPU, which is much slower.</span>
        )}
      </div>
      <AliasField id={model.id} value={draft.alias} fallback={null} onChange={(alias) => onChange({ alias })} />
      <CardField value={draft.card} metadata={model.metadata_card || null} fallback={null} onChange={(card) => onChange({ card })} />
      <Field
        label="Max context per agent (tokens)"
        hint="A number, or auto: what the model was trained for, capped to what fits in memory beside it."
        filled={draft.context !== ""}
        onReset={() => onChange({ context: "" })}
      >
        <input
          type="text"
          list="model-context-choices"
          value={draft.context}
          placeholder={eff.context_auto ? `auto (${eff.context})` : String(eff.context)}
          onChange={(e) => onChange({ context: e.target.value })}
        />
        <datalist id="model-context-choices">
          <option value="auto" />
        </datalist>
      </Field>
      {text("pool_tokens", "KV pool (tokens)", eff.pool_fixed ? eff.pool_tokens : null, {
        hint: eff.pool_fixed
          ? "Fixed: shared by every slot, at least the context."
          : `Empty: starts at ${eff.pool_tokens} and grows on demand up to the max context. A number fixes it.`,
      })}
      {text("slots", "Slots", eff.slots, { hint: "1 to 256 calls at once." })}
      {select("cache_type_k", "K cache type", cacheTypes, defaults.cache_type_k)}
      {select("cache_type_v", "V cache type", cacheTypes, defaults.cache_type_v)}
      {select("flash_attn", "Flash attention", FLASH, defaults.flash_attn)}
      {text("max_gpu_layers", "Max GPU layers", eff.max_gpu_layers, { disabled: vllm })}
      <Field
        label="Draft model"
        hint="Speculative decoding: a smaller installed model with the same tokenizer drafts tokens that this one checks, which speeds up generation. Empty: no draft. If it is missing, incompatible, or does not fit, the model runs without it."
        filled={draft.draft_model !== ""}
        onReset={() => onChange({ draft_model: "" })}
      >
        <input
          type="text"
          list="model-draft-choices"
          value={draft.draft_model}
          placeholder="none"
          disabled={vllm}
          onChange={(e) => onChange({ draft_model: e.target.value })}
        />
        <datalist id="model-draft-choices">
          {draftChoices.map((id) => (
            <option key={id} value={id} />
          ))}
        </datalist>
      </Field>
      {text("draft_max", "Draft max tokens", null, {
        disabled: vllm || draft.draft_model.trim() === "",
        hint: "Most tokens the draft proposes per step. Empty: llama.cpp's default.",
      })}
      {text("draft_min", "Draft min tokens", null, {
        disabled: vllm || draft.draft_model.trim() === "",
        hint: "Fewest tokens the draft proposes per step. Empty: llama.cpp's default.",
      })}
      {text("draft_gpu_layers", "Draft GPU layers", null, {
        disabled: vllm || draft.draft_model.trim() === "",
        hint: "Layers of the draft on the GPU; 0 keeps it in RAM. Empty: all of them.",
      })}
      {text("max_wait_s", "Max wait (s)", eff.max_wait_s)}
      {text("max_output_tokens", "Max output tokens", eff.max_output_tokens, {
        hint: "0 means uncapped (reserves the whole context).",
      })}
      <Field label="Extra arguments (one per line)" filled={draft.extra_args !== ""} onReset={() => onChange({ extra_args: "" })}>
        <textarea
          rows={3}
          value={draft.extra_args}
          placeholder={eff.extra_args.join("\n")}
          onChange={(e) => onChange({ extra_args: e.target.value })}
        />
      </Field>
      <div className="model-form-actions">
        <button type="button" onClick={onResetAll}>
          Reset model
        </button>
        {previewError && <span className="error">{previewError}</span>}
      </div>
    </div>
  );
}

function TableForm({
  row,
  draft,
  previewError,
  onChange,
  onResetAll,
}: {
  row: TableModelConfig;
  draft: TableDraft;
  previewError: string | null;
  onChange: (patch: Partial<TableDraft>) => void;
  onResetAll: () => void;
}) {
  const eff = row.effective;
  const priceField = (key: "input_usd_per_mtok" | "output_usd_per_mtok", label: string, hint?: string) => (
    <Field label={label} hint={hint} filled={draft[key] !== ""} onReset={() => onChange({ [key]: "" })}>
      <input
        type="text"
        inputMode="decimal"
        value={draft[key]}
        placeholder={eff[key] === null ? "free" : String(eff[key])}
        onChange={(e) => onChange({ [key]: e.target.value })}
      />
    </Field>
  );
  return (
    <div className="model-form-fields">
      <div className="model-summary">
        <span className="muted">{eff.alias ? `${row.id} · ${row.spec}` : row.spec}</span>
        {!row.endpoint_configured && <span className="warning">This row names an endpoint that is not configured.</span>}
      </div>
      <AliasField
        id={row.id}
        value={draft.alias}
        fallback={row.saved.alias === null ? eff.alias : null}
        onChange={(alias) => onChange({ alias })}
      />
      <CardField
        value={draft.card}
        metadata={null}
        fallback={row.saved.card === null ? eff.card : null}
        onChange={(card) => onChange({ card })}
      />
      <label className="radio model-check">
        <input type="checkbox" checked={draft.enabled ?? eff.enabled} onChange={(e) => onChange({ enabled: e.target.checked })} />
        Enabled (routing may use this model)
      </label>
      <Field label="Tier" filled={draft.tier !== ""} onReset={() => onChange({ tier: "" })}>
        <select value={draft.tier} onChange={(e) => onChange({ tier: e.target.value })}>
          <option value="">default ({eff.tier})</option>
          {TIERS.map((tier) => (
            <option key={tier} value={tier}>
              {tier}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Tags (comma-separated)" filled={draft.tags !== ""} onReset={() => onChange({ tags: "" })}>
        <input type="text" value={draft.tags} placeholder={eff.tags.join(", ")} onChange={(e) => onChange({ tags: e.target.value })} />
      </Field>
      <Field label="Context window (tokens)" filled={draft.context_window !== ""} onReset={() => onChange({ context_window: "" })}>
        <input
          type="text"
          inputMode="numeric"
          value={draft.context_window}
          placeholder={eff.context_window === null ? "unknown" : String(eff.context_window)}
          onChange={(e) => onChange({ context_window: e.target.value })}
        />
      </Field>
      {priceField("input_usd_per_mtok", "Input price ($/Mtok)", "Set both prices or neither.")}
      {priceField("output_usd_per_mtok", "Output price ($/Mtok)")}
      <div className="model-form-actions">
        <button type="button" onClick={onResetAll}>
          Reset model
        </button>
        {previewError && <span className="error">{previewError}</span>}
      </div>
    </div>
  );
}
