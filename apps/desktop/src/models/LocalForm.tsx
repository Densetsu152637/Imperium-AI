// The Local models tab: a model's runtime settings, memory and the use switch.

import { Fragment, type ReactNode } from "react";
import type { LocalModelConfig, ModelConfigResult } from "@imperium/protocol";
import { FLASH, MAX_ALIAS_CHARS, MAX_CARD_CHARS, type LocalDraft } from "./modelConfigDrafts";
import { localEstimate, poolHint } from "./modelMemory";
import { groupedSettings, LOCAL_GROUPS, LOCAL_SETTINGS } from "./modelSettingsTable";
import { Notice } from "../components/Notice";
import { Select } from "../components/Select";
import { GroupTable, SettingRow, MemoryTable, UseRow } from "./ConfigParts";

export function LocalForm({
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
          className="field"
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
        <Select
          aria-label={setting.label}
          className="setting-select"
          value={draft[key]}
          disabled={vllm}
          onChange={(value) => onChange({ [key]: value })}
          options={[{ value: "", label: `default (${fallback})` }, ...values.map((value) => ({ value, label: value }))]}
        />
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
          className="field"
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
          className="field"
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
          className="field"
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
      hint: eff.pool_fixed ? "Fixed: shared by every slot." : poolHint(eff),
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
          className="field"
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
          className="field"
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
        {!on && <Notice tone="warn">This model is off: it is never routed to, offered or chosen. Turn it on below to use it again.</Notice>}
        {model.reload_pending && <Notice tone="warn">Loaded with other settings; the new ones apply once no session uses it.</Notice>}
        {estimate.effective.pool_clamped && (
          <Notice tone="warn">
            The KV pool does not fit beside the weights on the GPU; it is cut to {estimate.effective.pool_tokens.toLocaleString()} tokens so
            every layer stays on the GPU.
          </Notice>
        )}
        {estimate.draft_in_use && (
          <span className="muted">
            Drafting with {estimate.draft_in_use} (speculative decoding); its memory is in the figures below.
          </span>
        )}
        {estimate.draft_notice && <Notice tone="warn">{estimate.draft_notice}</Notice>}
        {estimate.placement === "partial" && !vllm && (
          <Notice tone="warn">The weights do not fit on the GPU: some layers run on the CPU, which is much slower.</Notice>
        )}
      </div>
      <MemoryTable estimate={localEstimate(estimate)} updating={updating} failed={previewError !== null} />
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
        {previewError && <Notice tone="danger">{previewError}</Notice>}
      </div>
    </div>
  );
}
