// Model configuration: llama.cpp / vLLM settings of local models and the editable settings of
// model-table rows, drawn as tables: one row per setting (its name with an (i) icon that says in
// plain words what it affects, its value, a reset), grouped (modelSettingsTable.ts holds the
// rows and their explanations). A local model's table of estimated memory (modelMemory.ts) follows
// the settings as they are edited. "Use this model" switches a model off: it is greyed out, left
// out of the pickers, routing and escalation, and cannot be the default (modelAvailability.ts).
// Edits are saved through `models.set_config`; a debounced dry run supplies the live estimate and
// the server's validation message. It uses the Settings window's shell (SettingsModal): the models
// are its navigation, local ones and model-table rows in two groups.

import { Boxes, Cpu, Info, Search } from "lucide-react";
import { Fragment, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import type {
  LocalModelConfig,
  ModelConfigResult,
  ModelSetConfigParams,
  TableModelConfig,
  TableModelSettings,
  LocalLaunchSettings,
} from "@imperium/protocol";
import { Switch } from "./components/Switch";
import { matches, ModalNavGroup, ModalNavItem, SettingsModal } from "./components/SettingsModal";
import type { DaemonClient } from "./daemon/client";
import {
  defaultTurnedOff,
  disabledToSave,
  offSpecs,
  replacementDefaults,
  type AvailabilityInput,
  type Switches,
} from "./modelAvailability";
import {
  EMPTY_LOCAL,
  EMPTY_TABLE,
  FLASH,
  localDraft,
  localSettings,
  MAX_ALIAS_CHARS,
  MAX_CARD_CHARS,
  normLocal,
  normTable,
  same,
  tableDraft,
  tableSettings,
  TIERS,
  type LocalDraft,
  type TableDraft,
} from "./modelConfigDrafts";
import { estimateValue, localEstimate, rowEstimate, summaryLine, type MemoryEstimate } from "./modelMemory";
import {
  affectsText,
  ASPECT_LABELS,
  ESTIMATE_ROWS,
  groupedSettings,
  LOCAL_GROUPS,
  LOCAL_SETTINGS,
  TABLE_GROUPS,
  TABLE_SETTINGS,
  type SettingGroup,
  type SettingSpec,
} from "./modelSettingsTable";
import { arrangeModels, markOf } from "./modelPrefs";
import { useSettings } from "./settings";
import "./modelConfig.css";

export type ModelDialogTarget = { tab: "local" | "table"; id?: string };

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/** The spec a local model is used by. */
const localSpecOf = (model: LocalModelConfig): string => `${model.runtime}:${model.id}`;

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
  /** "Use this model" switches of local models, by spec, not saved yet. */
  const [localSwitches, setLocalSwitches] = useState<Record<string, boolean>>({});
  /** The model to make the default when the current default is switched off. */
  const [newDefault, setNewDefault] = useState("");
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

  // Which models are off, with the switches not saved yet.
  const availability: AvailabilityInput | null = useMemo(
    () =>
      config && {
        disabled: config.disabled ?? [],
        local: config.local.map(localSpecOf),
        rows: config.table.map((row) => ({ id: row.id, spec: row.spec, enabled: row.effective.enabled })),
        defaultModel: config.default_model ?? null,
      },
    [config],
  );
  const switches: Switches = useMemo(() => {
    const rows: Record<string, boolean> = {};
    for (const [id, draft] of Object.entries(tableDrafts)) {
      if (draft.enabled !== null) rows[id] = draft.enabled;
    }
    return { local: localSwitches, rows };
  }, [localSwitches, tableDrafts]);
  const off = useMemo(() => (availability ? offSpecs(availability, switches) : new Set<string>()), [availability, switches]);
  const defaultOff = availability ? defaultTurnedOff(availability, switches) : null;
  const replacements = useMemo(() => {
    if (!config || defaultOff === null) return [];
    const specs = [
      ...config.local.map((model) => ({ spec: localSpecOf(model), usable: true })),
      ...config.table.map((row) => ({ spec: row.spec, usable: row.endpoint_configured })),
    ];
    return replacementDefaults(specs, off, defaultOff);
  }, [config, defaultOff, off]);

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
    const params: ModelSetConfigParams = {
      local: [],
      table: [],
      hidden: null,
      disabled: config ? disabledToSave(config.disabled ?? [], switches) : null,
      dry_run: false,
    };
    for (const model of config?.local ?? []) {
      const settings = localChange(model);
      if (settings) params.local.push({ id: model.id, settings });
    }
    for (const row of config?.table ?? []) {
      const settings = tableChange(row);
      if (settings) params.table.push({ id: row.id, settings });
    }
    // The daemon refuses to switch off the default model: another one is chosen first.
    if (defaultOff !== null && newDefault === "") {
      throw new Error(`${defaultOff} is your default model: choose another default model before switching it off`);
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
    const params: ModelSetConfigParams = { local: [], table: [], hidden: null, disabled: null, dry_run: true };
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
    if (params.local.length === 0 && params.table.length === 0 && params.disabled === null) {
      if (close) onClose();
      return;
    }
    setSaving(true);
    try {
      // The new default first: the daemon refuses to switch the old one off while it is the default.
      if (defaultOff !== null && newDefault !== "") {
        await client.call("models.use", { model: newDefault });
      }
      const result = await client.call<ModelConfigResult>("models.set_config", params);
      onSaved();
      if (close) {
        onClose();
        return;
      }
      setConfig(result);
      setLocalDrafts({});
      setTableDrafts({});
      setLocalSwitches({});
      setNewDefault("");
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
        return model ? localChange(model) !== null || localSpecOf(model) in localSwitches : false;
      }
      const row = tableById.get(id);
      return row ? tableChange(row) !== null : false;
    } catch {
      return true;
    }
  };
  const dirty =
    config !== null &&
    (config.local.some((m) => changed(m.id, "local")) ||
      config.table.some((r) => changed(r.id, "table")) ||
      disabledToSave(config.disabled ?? [], switches) !== null);

  const setLocal = (model: LocalModelConfig, patch: Partial<LocalDraft>) =>
    setLocalDrafts((all) => ({ ...all, [model.id]: { ...(all[model.id] ?? localDraft(model.saved)), ...patch } }));
  const setTable = (row: TableModelConfig, patch: Partial<TableDraft>) =>
    setTableDrafts((all) => ({ ...all, [row.id]: { ...(all[row.id] ?? tableDraft(row.saved)), ...patch } }));
  /** Switches a local model: back to what is saved when it matches, so it is not "edited". */
  const switchLocal = (model: LocalModelConfig, on: boolean) =>
    setLocalSwitches((all) => {
      const { [localSpecOf(model)]: _old, ...rest } = all;
      return on === (config?.disabled ?? []).every((spec) => spec !== localSpecOf(model)) ? rest : { ...rest, [localSpecOf(model)]: on };
    });

  // Favourites first and hidden models last, as in the model panels (modelPrefs.ts).
  const specOf = (item: LocalModelConfig | TableModelConfig) => ("spec" in item ? item.spec : localSpecOf(item));
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
    // A table row is off by its own switch (or its model's); a local model by the list.
    const isOff = row
      ? (config?.disabled ?? []).includes(row.spec) || !(tableDrafts[row.id]?.enabled ?? row.effective.enabled)
      : off.has(specOf(item));
    return (
      <ModalNavItem
        key={`${kind}:${item.id}`}
        icon={kind === "local" ? Cpu : Boxes}
        active={tab === kind && selected === item.id}
        dimmed={mark === "hidden" || isOff}
        title={alias ? `${alias} (${item.id})` : item.id}
        onClick={() => {
          setTab(kind);
          setSelected(item.id);
        }}
        badges={
          <>
            {isOff && <span className="badge off">off</span>}
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

  /** The choice shown with the switch of the default model when it is being switched off. */
  const defaultChoice = (spec: string): ReactNode =>
    defaultOff === spec ? (
      <div className="model-default-choice">
        <span>This is your default model for new sessions. Choose the one to use instead:</span>
        <select aria-label="New default model" value={newDefault} onChange={(event) => setNewDefault(event.target.value)}>
          <option value="">choose a model…</option>
          {replacements.map((replacement) => (
            <option key={replacement} value={replacement}>
              {replacement}
            </option>
          ))}
        </select>
        {replacements.length === 0 && <span className="warning">No other model is on to take over.</span>}
      </div>
    ) : null;

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
          updating={preview === null && previewError === null && selectedLocalDraft !== undefined && changed(selectedLocal.id, "local")}
          on={!off.has(localSpecOf(selectedLocal))}
          onSwitch={(on) => switchLocal(selectedLocal, on)}
          defaultChoice={defaultChoice(localSpecOf(selectedLocal))}
          onChange={(patch) => setLocal(selectedLocal, patch)}
          onResetAll={() => {
            setLocalDrafts((all) => ({ ...all, [selectedLocal.id]: EMPTY_LOCAL }));
          }}
        />
      )}
      {selectedTable && config && (
        <TableForm
          row={selectedTable}
          draft={tableDrafts[selectedTable.id] ?? tableDraft(selectedTable.saved)}
          local={config.local}
          previewError={previewError}
          modelOff={(config.disabled ?? []).includes(selectedTable.spec)}
          on={!(config.disabled ?? []).includes(selectedTable.spec) && (tableDrafts[selectedTable.id]?.enabled ?? selectedTable.effective.enabled)}
          defaultChoice={defaultChoice(selectedTable.spec)}
          onChange={(patch) => setTable(selectedTable, patch)}
          onResetAll={() => setTableDrafts((all) => ({ ...all, [selectedTable.id]: EMPTY_TABLE }))}
        />
      )}
    </SettingsModal>
  );
}

/**
 * The (i) icon beside a setting: a keyboard-focusable button whose popover says in plain words
 * what the setting affects. It opens on hover and on focus, stays while the button has focus, and
 * Esc closes it (without closing the dialog). The text is the button's description
 * (`aria-describedby`), so a screen reader reads it with the button.
 */
function InfoTip({ label, children }: { label: string; children: ReactNode }) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const focused = useRef(false);
  return (
    <span className="info-tip" onMouseEnter={() => setOpen(true)} onMouseLeave={() => setOpen(focused.current)}>
      <button
        type="button"
        className="info-tip-button"
        aria-label={`What ${label} affects`}
        aria-describedby={id}
        aria-expanded={open}
        onClick={() => setOpen(true)}
        onFocus={() => {
          focused.current = true;
          setOpen(true);
        }}
        onBlur={() => {
          focused.current = false;
          setOpen(false);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape" && open) {
            // Closes the popover only: the dialog's own Esc handler sees the prevented event.
            event.preventDefault();
            setOpen(false);
          }
        }}
      >
        <Info aria-hidden="true" />
      </button>
      <span role="tooltip" id={id} className="info-tip-pop" hidden={!open}>
        {children}
      </span>
    </span>
  );
}

/** One group of settings as a table, with its title and note as the caption. */
function GroupTable({ group, children }: { group: SettingGroup; children: ReactNode }) {
  return (
    <table className="model-table">
      <caption>
        <span className="model-table-title">{group.title}</span>
        {group.note && <small className="muted">{group.note}</small>}
      </caption>
      <thead className="visually-hidden">
        <tr>
          <th scope="col">Setting</th>
          <th scope="col">Value</th>
          <th scope="col">Reset</th>
        </tr>
      </thead>
      <tbody>{children}</tbody>
    </table>
  );
}

/** A table row: the setting's name with its (i), the value control, and a reset when it is set. */
function SettingRow({
  spec,
  filled,
  onReset,
  hint,
  unavailable,
  children,
}: {
  spec: Pick<SettingSpec<string>, "label" | "help" | "affects">;
  filled: boolean;
  onReset: () => void;
  hint?: ReactNode;
  /** Why the value cannot be changed (shown instead of the hint). */
  unavailable?: string;
  children: ReactNode;
}) {
  return (
    <tr className={`model-setting-row${unavailable ? " unavailable" : ""}`}>
      <th scope="row">
        <span className="model-setting-name">
          {spec.label}
          <InfoTip label={spec.label}>
            <span>{spec.help}</span>
            <span className="info-tip-affects">{affectsText(spec.affects)}</span>
          </InfoTip>
        </span>
        <small className="model-setting-aspects muted">{spec.affects.map((aspect) => ASPECT_LABELS[aspect]).join(" · ")}</small>
      </th>
      <td className="model-setting-value">
        {children}
        {(unavailable ?? hint) && <small className="muted">{unavailable ?? hint}</small>}
      </td>
      <td className="model-setting-reset">
        {filled && !unavailable && (
          <button type="button" className="ghost reset" onClick={onReset} title="Use the default" aria-label={`Reset ${spec.label}`}>
            reset
          </button>
        )}
      </td>
    </tr>
  );
}

/** The estimated memory of a model as a table: "≈ estimate", updating as the settings change. */
function MemoryTable({ estimate, updating, remoteNote }: { estimate: MemoryEstimate; updating: boolean; remoteNote?: string }) {
  const caption = (
    <caption>
      <span className="model-table-title">
        Estimated memory <span className="badge estimate">≈ estimate</span>
        {updating && <span className="muted model-estimate-updating"> updating…</span>}
      </span>
      <small className="muted">
        {estimate.kind === "remote"
          ? "A hosted model runs on the provider's computers."
          : "What this computer would use with the settings below, from the same planner that loads the model; it updates as you change them."}
      </small>
    </caption>
  );
  if (estimate.kind === "remote") {
    return (
      <table className="model-table model-memory">
        {caption}
        <tbody>
          <tr className="model-setting-row">
            <th scope="row">
              <span className="model-setting-name">
                Memory on this computer
                <InfoTip label="memory on this computer">
                  <span>
                    {remoteNote ??
                      "This model runs on the provider's servers, so it uses no graphics card or system memory here. Only your network and the provider's price and limits matter."}
                  </span>
                </InfoTip>
              </span>
            </th>
            <td className="model-setting-value">runs remotely</td>
            <td className="model-setting-reset" />
          </tr>
        </tbody>
      </table>
    );
  }
  return (
    <table className={`model-table model-memory${updating ? " updating" : ""}`}>
      {caption}
      <tbody>
        {ESTIMATE_ROWS.map((row) => (
          <tr key={row.key} className={`model-setting-row${estimate.fits ? "" : " warn"}`}>
            <th scope="row">
              <span className="model-setting-name">
                {row.label}
                <InfoTip label={row.label}>
                  <span>{row.help}</span>
                </InfoTip>
              </span>
            </th>
            <td className="model-setting-value">
              <output>{estimateValue(estimate, row.key)}</output>
            </td>
            <td className="model-setting-reset" />
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** The "Use this model" row: a switch, and what being off means. */
function UseRow({
  spec,
  on,
  onSwitch,
  locked,
  defaultChoice,
}: {
  spec: Pick<SettingSpec<string>, "label" | "help" | "affects">;
  on: boolean;
  onSwitch: (on: boolean) => void;
  /** Why the switch cannot be changed. */
  locked?: string;
  defaultChoice: ReactNode;
}) {
  return (
    <SettingRow
      spec={spec}
      filled={false}
      onReset={() => {}}
      hint={
        locked ??
        (on ? "On: routing, the pickers and agents may use it." : "Off: greyed out, never routed or offered, and it cannot be the default.")
      }
    >
      <Switch checked={on} onChange={onSwitch} label={`${spec.label}`} disabled={locked !== undefined} />
      {defaultChoice}
    </SettingRow>
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
  updating,
  on,
  onSwitch,
  defaultChoice,
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
  /** The estimate is waiting for the daemon's answer to the latest edit. */
  updating: boolean;
  on: boolean;
  onSwitch: (on: boolean) => void;
  defaultChoice: ReactNode;
  onChange: (patch: Partial<LocalDraft>) => void;
  onResetAll: () => void;
}) {
  const vllm = model.runtime === "vllm";
  const eff = model.effective;
  const estimate = preview ?? model;
  const spec = (key: string) => {
    const found = LOCAL_SETTINGS.find((setting) => setting.key === key);
    if (!found) throw new Error(`no setting ${key}`);
    return found;
  };
  const noVllm = (key: string): string | undefined => (vllm && spec(key).llamacppOnly ? "Not used by vLLM models." : undefined);
  const text = (key: keyof LocalDraft, placeholder: number | null, options: { disabled?: boolean; hint?: ReactNode } = {}) => {
    const setting = spec(key);
    return (
      <SettingRow spec={setting} filled={draft[key] !== ""} onReset={() => onChange({ [key]: "" })} hint={options.hint} unavailable={noVllm(key)}>
        <input
          type="text"
          inputMode="numeric"
          aria-label={setting.label}
          value={draft[key]}
          placeholder={placeholder === null ? "planner decides" : String(placeholder)}
          disabled={options.disabled || noVllm(key) !== undefined}
          onChange={(e) => onChange({ [key]: e.target.value })}
        />
      </SettingRow>
    );
  };
  const select = (key: "cache_type_k" | "cache_type_v" | "flash_attn", values: readonly string[], fallback: string) => {
    const setting = spec(key);
    return (
      <SettingRow spec={setting} filled={draft[key] !== ""} onReset={() => onChange({ [key]: "" })} unavailable={noVllm(key)}>
        <select aria-label={setting.label} value={draft[key]} disabled={vllm} onChange={(e) => onChange({ [key]: e.target.value })}>
          <option value="">default ({fallback})</option>
          {values.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
      </SettingRow>
    );
  };
  const draftOff = vllm || draft.draft_model.trim() === "";
  const rows: Record<string, ReactNode> = {
    enabled: <UseRow key="enabled" spec={spec("enabled")} on={on} onSwitch={onSwitch} defaultChoice={defaultChoice} />,
    alias: (
      <SettingRow
        key="alias"
        spec={spec("alias")}
        filled={draft.alias !== ""}
        onReset={() => onChange({ alias: "" })}
        hint={`Empty shows the id (${model.id}).`}
      >
        <input
          type="text"
          aria-label={spec("alias").label}
          value={draft.alias}
          placeholder={model.id}
          maxLength={MAX_ALIAS_CHARS}
          spellCheck={false}
          onChange={(e) => onChange({ alias: e.target.value })}
        />
      </SettingRow>
    ),
    card: (
      <SettingRow key="card" spec={spec("card")} filled={draft.card !== ""} onReset={() => onChange({ card: "" })}>
        <textarea
          rows={4}
          aria-label={spec("card").label}
          value={draft.card}
          maxLength={MAX_CARD_CHARS}
          placeholder="Good at: coding, tool use. Notes: fast; weak at long documents."
          onChange={(e) => onChange({ card: e.target.value })}
        />
        {model.metadata_card && (
          <details className="model-card-metadata">
            <summary>
              From the model's metadata{" "}
              {draft.card === "" && (
                <button type="button" className="ghost reset" onClick={() => onChange({ card: model.metadata_card })}>
                  copy into the card
                </button>
              )}
            </summary>
            <pre className="muted">{model.metadata_card}</pre>
          </details>
        )}
      </SettingRow>
    ),
    context: (
      <SettingRow key="context" spec={spec("context")} filled={draft.context !== ""} onReset={() => onChange({ context: "" })} hint="A number, or auto.">
        <input
          type="text"
          list="model-context-choices"
          aria-label={spec("context").label}
          value={draft.context}
          placeholder={eff.context_auto ? `auto (${eff.context})` : String(eff.context)}
          onChange={(e) => onChange({ context: e.target.value })}
        />
        <datalist id="model-context-choices">
          <option value="auto" />
        </datalist>
      </SettingRow>
    ),
    pool_tokens: text("pool_tokens", eff.pool_fixed ? eff.pool_tokens : null, {
      hint: eff.pool_fixed ? "Fixed: shared by every slot." : `Empty: starts at ${eff.pool_tokens} and grows on demand up to the max context.`,
    }),
    slots: text("slots", eff.slots),
    cache_type_k: select("cache_type_k", cacheTypes, defaults.cache_type_k),
    cache_type_v: select("cache_type_v", cacheTypes, defaults.cache_type_v),
    flash_attn: select("flash_attn", FLASH, defaults.flash_attn),
    max_gpu_layers: text("max_gpu_layers", eff.max_gpu_layers),
    draft_model: (
      <SettingRow
        key="draft_model"
        spec={spec("draft_model")}
        filled={draft.draft_model !== ""}
        onReset={() => onChange({ draft_model: "" })}
        unavailable={noVllm("draft_model")}
      >
        <input
          type="text"
          list="model-draft-choices"
          aria-label={spec("draft_model").label}
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
      </SettingRow>
    ),
    draft_max: text("draft_max", null, { disabled: draftOff, hint: draftOff && !vllm ? "Needs a draft model." : undefined }),
    draft_min: text("draft_min", null, { disabled: draftOff, hint: draftOff && !vllm ? "Needs a draft model." : undefined }),
    draft_gpu_layers: text("draft_gpu_layers", null, { disabled: draftOff, hint: draftOff && !vllm ? "Needs a draft model." : undefined }),
    max_wait_s: text("max_wait_s", eff.max_wait_s),
    max_output_tokens: text("max_output_tokens", eff.max_output_tokens),
    extra_args: (
      <SettingRow key="extra_args" spec={spec("extra_args")} filled={draft.extra_args !== ""} onReset={() => onChange({ extra_args: "" })}>
        <textarea
          rows={3}
          aria-label={spec("extra_args").label}
          value={draft.extra_args}
          placeholder={eff.extra_args.join("\n")}
          onChange={(e) => onChange({ extra_args: e.target.value })}
        />
      </SettingRow>
    ),
  };
  return (
    <div className={`model-config${on ? "" : " model-off"}`}>
      <div className="model-summary">
        <span className="muted">
          {model.saved.alias ? `${model.id} · ` : ""}
          {model.runtime}
          {model.cpu_lane ? " · CPU lane" : ""}
          {model.context_length ? ` · trained for ${model.context_length.toLocaleString()} tokens` : ""}
          {` · up to ${estimate.effective.context.toLocaleString()} per agent${estimate.effective.context_auto ? " (auto)" : ""}`}
        </span>
        {!on && <span className="warning">This model is off: it is never routed to, offered or chosen. Turn it on below to use it again.</span>}
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
            Drafting with {estimate.draft_in_use} (speculative decoding); its memory is in the figures below.
          </span>
        )}
        {estimate.draft_notice && <span className="warning">{estimate.draft_notice}</span>}
        {estimate.placement === "partial" && !vllm && (
          <span className="warning">The weights do not fit on the GPU: some layers run on the CPU, which is much slower.</span>
        )}
      </div>
      <MemoryTable estimate={localEstimate(estimate)} updating={updating} />
      {groupedSettings(LOCAL_GROUPS, LOCAL_SETTINGS).map(({ group, settings }) => (
        <GroupTable key={group.id} group={group}>
          {settings.map((setting) => (
            <Fragment key={setting.key}>{rows[setting.key]}</Fragment>
          ))}
        </GroupTable>
      ))}
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
  local,
  previewError,
  modelOff,
  on,
  defaultChoice,
  onChange,
  onResetAll,
}: {
  row: TableModelConfig;
  draft: TableDraft;
  local: LocalModelConfig[];
  previewError: string | null;
  /** The row's model is switched off in the local models list, which switches the row off. */
  modelOff: boolean;
  on: boolean;
  defaultChoice: ReactNode;
  onChange: (patch: Partial<TableDraft>) => void;
  onResetAll: () => void;
}) {
  const eff = row.effective;
  const spec = (key: string) => {
    const found = TABLE_SETTINGS.find((setting) => setting.key === key);
    if (!found) throw new Error(`no setting ${key}`);
    return found;
  };
  const priceField = (key: "input_usd_per_mtok" | "output_usd_per_mtok", hint?: string) => (
    <SettingRow key={key} spec={spec(key)} filled={draft[key] !== ""} onReset={() => onChange({ [key]: "" })} hint={hint}>
      <input
        type="text"
        inputMode="decimal"
        aria-label={spec(key).label}
        value={draft[key]}
        placeholder={eff[key] === null ? "free" : String(eff[key])}
        onChange={(e) => onChange({ [key]: e.target.value })}
      />
    </SettingRow>
  );
  const rows: Record<string, ReactNode> = {
    enabled: (
      <UseRow
        key="enabled"
        spec={spec("enabled")}
        on={on}
        locked={modelOff ? "Its model is switched off in the local models list; switch that on first." : undefined}
        // Back to "not set" when it matches the configured value, so the row is not "edited".
        onSwitch={(value) => onChange({ enabled: value === eff.enabled && row.saved.enabled === null ? null : value })}
        defaultChoice={defaultChoice}
      />
    ),
    alias: (
      <SettingRow
        key="alias"
        spec={spec("alias")}
        filled={draft.alias !== ""}
        onReset={() => onChange({ alias: "" })}
        hint={`Empty shows ${row.saved.alias === null && eff.alias ? "the configured alias" : "the id"} (${row.id}).`}
      >
        <input
          type="text"
          aria-label={spec("alias").label}
          value={draft.alias}
          placeholder={(row.saved.alias === null ? eff.alias : null) ?? row.id}
          maxLength={MAX_ALIAS_CHARS}
          spellCheck={false}
          onChange={(e) => onChange({ alias: e.target.value })}
        />
      </SettingRow>
    ),
    card: (
      <SettingRow key="card" spec={spec("card")} filled={draft.card !== ""} onReset={() => onChange({ card: "" })}>
        <textarea
          rows={4}
          aria-label={spec("card").label}
          value={draft.card}
          maxLength={MAX_CARD_CHARS}
          placeholder={(row.saved.card === null ? eff.card : null) ?? "Good at: coding, tool use. Notes: fast; weak at long documents."}
          onChange={(e) => onChange({ card: e.target.value })}
        />
      </SettingRow>
    ),
    tier: (
      <SettingRow key="tier" spec={spec("tier")} filled={draft.tier !== ""} onReset={() => onChange({ tier: "" })}>
        <select aria-label={spec("tier").label} value={draft.tier} onChange={(e) => onChange({ tier: e.target.value })}>
          <option value="">default ({eff.tier})</option>
          {TIERS.map((tier) => (
            <option key={tier} value={tier}>
              {tier}
            </option>
          ))}
        </select>
      </SettingRow>
    ),
    tags: (
      <SettingRow key="tags" spec={spec("tags")} filled={draft.tags !== ""} onReset={() => onChange({ tags: "" })}>
        <input
          type="text"
          aria-label={spec("tags").label}
          value={draft.tags}
          placeholder={eff.tags.join(", ")}
          onChange={(e) => onChange({ tags: e.target.value })}
        />
      </SettingRow>
    ),
    context_window: (
      <SettingRow key="context_window" spec={spec("context_window")} filled={draft.context_window !== ""} onReset={() => onChange({ context_window: "" })}>
        <input
          type="text"
          inputMode="numeric"
          aria-label={spec("context_window").label}
          value={draft.context_window}
          placeholder={eff.context_window === null ? "unknown" : String(eff.context_window)}
          onChange={(e) => onChange({ context_window: e.target.value })}
        />
      </SettingRow>
    ),
    input_usd_per_mtok: priceField("input_usd_per_mtok", "Set both prices or neither."),
    output_usd_per_mtok: priceField("output_usd_per_mtok"),
  };
  const estimate = rowEstimate(row, local);
  return (
    <div className={`model-config${on ? "" : " model-off"}`}>
      <div className="model-summary">
        <span className="muted">{eff.alias ? `${row.id} · ${row.spec}` : row.spec}</span>
        {!on && <span className="warning">This model is off: it is never routed to, offered or chosen. Turn it on below to use it again.</span>}
        {!row.endpoint_configured && <span className="warning">This row names an endpoint that is not configured.</span>}
        {estimate.kind === "local" && <span className="muted">Runs locally: {summaryLine(estimate)}.</span>}
      </div>
      <MemoryTable estimate={estimate} updating={false} />
      {groupedSettings(TABLE_GROUPS, TABLE_SETTINGS).map(({ group, settings }) => (
        <GroupTable key={group.id} group={group}>
          {settings.map((setting) => (
            <Fragment key={setting.key}>{rows[setting.key]}</Fragment>
          ))}
        </GroupTable>
      ))}
      <div className="model-form-actions">
        <button type="button" onClick={onResetAll}>
          Reset model
        </button>
        {previewError && <span className="error">{previewError}</span>}
      </div>
    </div>
  );
}
