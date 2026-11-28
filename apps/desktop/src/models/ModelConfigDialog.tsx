// Model configuration: llama.cpp / vLLM settings of local models and the editable settings of
// model-table rows, drawn as tables: one row per setting (its name with an (i) icon that says in
// plain words what it affects, its value, a reset), grouped (modelSettingsTable.ts holds the
// rows and their explanations). A local model's table of estimated memory (modelMemory.ts) follows
// the settings as they are edited. "Use this model" switches a model off: it is greyed out, left
// out of the pickers, routing and escalation, and cannot be the default (modelAvailability.ts).
// Edits are saved through `models.set_config`; a debounced dry run supplies the live estimate and
// the server's validation message. It uses the Settings window's shell (Dialog): the models
// are its navigation, local ones and model-table rows in two groups.

import { Boxes, Cpu, Search } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import type { LocalModelConfig, ModelConfigResult, ModelSetConfigParams, TableModelConfig, TableModelSettings, LocalLaunchSettings } from "@imperium/protocol";
import { matches, DialogNavGroup, DialogNavItem, Dialog } from "../components/Dialog";
import { Badge } from "../components/Badge";
import { EmptyState } from "../components/EmptyState";
import { LoadingState } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { Select } from "../components/Select";
import type { DaemonClient } from "../daemon/client";
import { defaultTurnedOff, disabledToSave, offSpecs, replacementDefaults, type AvailabilityInput, type Switches } from "./modelAvailability";
import { EMPTY_LOCAL, EMPTY_TABLE, localDraft, localSettings, normLocal, normTable, same, tableDraft, tableSettings, type LocalDraft, type TableDraft } from "./modelConfigDrafts";
import { arrangeModels, markOf } from "./modelPrefs";
import { useSettings } from "../settings/settings";
import "./modelConfig.css";
import { LocalForm } from "./LocalForm";
import { TableForm } from "./TableForm";
import { callAsUser } from "../daemon/userCall";

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
    let switchedDefault: string | null = null;
    try {
      // The new default first: the daemon refuses to switch the old one off while it is the default.
      if (defaultOff !== null && newDefault !== "") {
        await client.call("models.use", { model: newDefault });
        switchedDefault = newDefault;
      }
      const result = await callAsUser<ModelConfigResult>(client, "models.set_config", params);
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
      // The default was changed by the first call even though the rest failed.
      if (switchedDefault !== null) {
        onSaved();
        setError(`${message(e)} (your default model was already changed to ${switchedDefault})`);
      } else {
        setError(message(e));
      }
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
      <DialogNavItem
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
            {isOff && <Badge className="off">off</Badge>}
            {mark === "favourite" && <Badge>favourite</Badge>}
            {mark === "hidden" && <Badge className="off">hidden</Badge>}
            {local && <Badge>{local.runtime}</Badge>}
            {row && <Badge tone={row.location === "local" ? "ok" : "accent"}>{row.location}</Badge>}
            {local?.loaded && <Badge tone="ok">loaded</Badge>}
            {local?.reload_pending && <Badge>reload pending</Badge>}
            {changed(item.id, kind) && <Badge tone="accent">edited</Badge>}
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
      </DialogNavItem>
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
      <DialogNavGroup label="Local models">
        {localItems.map((item) => navItem(item, "local"))}
        {config && localItems.length === 0 && (
          <span className="settings-nav-empty">{query.trim() ? "No matches." : "No local models found."}</span>
        )}
      </DialogNavGroup>
      <DialogNavGroup label="Model table">
        {tableItems.map((item) => navItem(item, "table"))}
        {config && tableItems.length === 0 && (
          <span className="settings-nav-empty">{query.trim() ? "No matches." : "The model table is empty."}</span>
        )}
      </DialogNavGroup>
    </>
  );

  const footer = (
    <>
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
        <Select
          aria-label="New default model"
          className="setting-select"
          value={newDefault}
          onChange={setNewDefault}
          options={[{ value: "", label: "choose a model…" }, ...replacements.map((replacement) => ({ value: replacement, label: replacement }))]}
        />
        {replacements.length === 0 && <Notice tone="warn">No other model is on to take over.</Notice>}
      </div>
    ) : null;

  return (
    <Dialog
      label="Models"
      title={selected ? (selectedAlias ?? selected) : "Models"}
      nav={nav}
      footer={footer}
      notice={error ? <Notice tone="danger">{error}</Notice> : undefined}
      onClose={onClose}
      onKeyDown={(event) => {
        // Ctrl+S (Cmd+S on macOS) applies without closing.
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
          event.preventDefault();
          if (dirty) void save(false);
        }
      }}
    >
      {!config && !error && <LoadingState />}
      {config && !selectedLocal && !selectedTable && <EmptyState title="No models to configure" />}
      {selectedLocal && config && (
        <LocalForm
          model={selectedLocal}
          draftChoices={config.local.filter((m) => m.runtime === "llamacpp" && m.id !== selectedLocal.id).map((m) => m.id)}
          draft={localDrafts[selectedLocal.id] ?? localDraft(selectedLocal.saved)}
          defaults={config.defaults}
          cacheTypes={config.cache_types}
          preview={preview}
          previewError={previewError}
          updating={preview === null && selectedLocalDraft !== undefined && changed(selectedLocal.id, "local")}
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
    </Dialog>
  );
}
