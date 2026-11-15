// One settings field: its label, description and control.

import type { CSSProperties } from "react";
import { Select } from "../components/Select";
import { Row } from "../components/SettingsRow";
import { Switch as Toggle } from "../components/Switch";
import { DEFAULT_SETTINGS, type AppSettings } from ".";
import type { Field } from "./fields";

export function FieldRow({
  field,
  value,
  onChange,
}: {
  field: Field;
  value: AppSettings[keyof AppSettings];
  onChange: <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => void;
}) {
  const { control } = field;
  const changed = value !== DEFAULT_SETTINGS[field.key];
  const badge = changed ? (
    <button className="ghost reset-link" onClick={() => onChange(field.key, DEFAULT_SETTINGS[field.key])} title="Back to the default">
      reset
    </button>
  ) : null;
  return (
    <Row label={field.label} description={field.description} badge={badge}>
      {control.type === "toggle" && (
        <Toggle checked={Boolean(value)} onChange={(next) => onChange(field.key, next as never)} label={field.label} />
      )}
      {control.type === "select" && (
        <Select
          value={String(value)}
          options={control.options}
          onChange={(next) => onChange(field.key, next as never)}
          aria-label={field.label}
        />
      )}
      {control.type === "swatches" && (
        <div className="swatches" role="radiogroup" aria-label={field.label}>
          {control.options.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={value === option.value}
              aria-label={option.label}
              title={option.label}
              className={`swatch${value === option.value ? " selected" : ""}`}
              style={{ "--swatch": option.color } as CSSProperties}
              onClick={() => onChange(field.key, option.value as never)}
            />
          ))}
        </div>
      )}
      {control.type === "slider" && (
        <label className="slider">
          <input
            type="range"
            min={control.min}
            max={control.max}
            step={control.step}
            value={Number(value)}
            onChange={(event) => onChange(field.key, Number(event.target.value) as never)}
            aria-label={field.label}
          />
          <output>{control.format(Number(value))}</output>
        </label>
      )}
    </Row>
  );
}
