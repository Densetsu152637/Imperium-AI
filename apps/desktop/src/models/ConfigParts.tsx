// The rows and tables the model configuration forms share.

import { Info } from "lucide-react";
import { useEffect, useRef, type ReactNode } from "react";
import { Badge } from "../components/Badge";
import { Switch } from "../components/Switch";
import { Tooltip } from "../components/Tooltip";
import { estimateValue, kvLabel, type MemoryEstimate } from "./modelMemory";
import { affectsText, ASPECT_LABELS, ESTIMATE_ROWS, type SettingGroup, type SettingSpec } from "./modelSettingsTable";

/**
 * The (i) icon beside a setting: a keyboard-focusable button whose tooltip says in plain words what
 * the setting affects. The shared Tooltip shows it on hover and, at once, on keyboard focus, and
 * links it to the button while it shows (`aria-describedby`). Esc closes the tooltip only: this
 * listener runs before the tooltip's own and prevents the key, which the dialog's Esc handler
 * respects, so the dialog stays open.
 */
function InfoTip({ label, children }: { label: string; children: ReactNode }) {
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      // `aria-describedby` is on the button only while its tooltip shows.
      if (event.key === "Escape" && button.current?.hasAttribute("aria-describedby")) event.preventDefault();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, []);
  return (
    <Tooltip text={children}>
      <button ref={button} type="button" className="info-tip-button" aria-label={`What ${label} affects`}>
        <Info aria-hidden="true" />
      </button>
    </Tooltip>
  );
}

/** One group of settings as a table, with its title and note as the caption. */
export function GroupTable({ group, children }: { group: SettingGroup; children: ReactNode }) {
  return (
    <table className="table model-table">
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
export function SettingRow({
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
            <span className="info-tip-help">{spec.help}</span>
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
          <button type="button" className="ghost small reset" onClick={onReset} title="Use the default" aria-label={`Reset ${spec.label}`}>
            reset
          </button>
        )}
      </td>
    </tr>
  );
}

/** The estimated memory of a model as a table: "≈ estimate", updating as the settings change. */
export function MemoryTable({
  estimate,
  updating,
  failed = false,
  remoteNote,
}: {
  estimate: MemoryEstimate;
  updating: boolean;
  /** The last dry run failed: the figures are those before the edit. */
  failed?: boolean;
  remoteNote?: string;
}) {
  const caption = (
    <caption>
      <span className="model-table-title">
        Estimated memory <Badge className="estimate">≈ estimate</Badge>
        {failed ? (
          <span className="muted model-estimate-updating"> out of date: fix the settings below</span>
        ) : (
          updating && <span className="muted model-estimate-updating"> updating…</span>
        )}
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
      <table className="table model-table model-memory">
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
    <table className={`table model-table model-memory${updating || failed ? " updating" : ""}`}>
      {caption}
      <tbody>
        {ESTIMATE_ROWS.map((row) => {
          const label = row.key === "kv" ? kvLabel(estimate) : row.label;
          return (
            <tr key={row.key} className={`model-setting-row${estimate.fits ? "" : " warn"}`}>
              <th scope="row">
                <span className="model-setting-name">
                  {label}
                  <InfoTip label={label}>
                    <span>{row.help}</span>
                  </InfoTip>
                </span>
              </th>
              <td className="model-setting-value">
                <output aria-live="off">{estimateValue(estimate, row.key)}</output>
              </td>
              <td className="model-setting-reset" />
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/** The "Use this model" row: a switch, and what being off means. */
export function UseRow({
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
