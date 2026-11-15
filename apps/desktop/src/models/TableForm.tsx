// The Models tab: a model-table row's endpoint, name and settings.

import { Fragment, type ReactNode } from "react";
import type { LocalModelConfig, TableModelConfig } from "@imperium/protocol";
import { MAX_ALIAS_CHARS, MAX_CARD_CHARS, TIERS, type TableDraft } from "../modelConfigDrafts";
import { rowEstimate, summaryLine } from "../modelMemory";
import { groupedSettings, TABLE_GROUPS, TABLE_SETTINGS } from "../modelSettingsTable";
import { GroupTable, SettingRow, MemoryTable, UseRow } from "./ConfigParts";

export function TableForm({
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
