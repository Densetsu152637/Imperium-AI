// Display settings: UI scale, prose and code font scale, density, and line length.

import type { Density, DisplaySettings } from "./settings";

type Props = { settings: DisplaySettings; onChange: (settings: DisplaySettings) => void; onClose: () => void };

function Slider({
  label,
  value,
  min,
  max,
  step,
  format,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  format: (value: number) => string;
  onChange: (value: number) => void;
}) {
  return (
    <label className="setting">
      <span>
        {label} <output>{format(value)}</output>
      </span>
      <input type="range" min={min} max={max} step={step} value={value} onChange={(e) => onChange(Number(e.target.value))} />
    </label>
  );
}

const percent = (value: number) => `${Math.round(value * 100)}%`;

export function SettingsMenu({ settings, onChange, onClose }: Props) {
  const set = (patch: Partial<DisplaySettings>) => onChange({ ...settings, ...patch });
  return (
    <div className="settings-menu" role="dialog" aria-label="Display settings">
      <Slider label="UI scale" value={settings.uiScale} min={0.5} max={3} step={0.05} format={percent} onChange={(uiScale) => set({ uiScale })} />
      <Slider
        label="Prose text"
        value={settings.proseScale}
        min={0.5}
        max={3}
        step={0.05}
        format={percent}
        onChange={(proseScale) => set({ proseScale })}
      />
      <Slider label="Code text" value={settings.codeScale} min={0.5} max={3} step={0.05} format={percent} onChange={(codeScale) => set({ codeScale })} />
      <Slider
        label="Line length"
        value={settings.maxProseCh}
        min={50}
        max={200}
        step={5}
        format={(value) => `${value} ch`}
        onChange={(maxProseCh) => set({ maxProseCh })}
      />
      <fieldset className="setting">
        <legend>Density</legend>
        {(["compact", "comfortable", "spacious"] as Density[]).map((density) => (
          <label key={density} className="radio">
            <input type="radio" name="density" checked={settings.density === density} onChange={() => set({ density })} />
            {density}
          </label>
        ))}
      </fieldset>
      <button className="primary" onClick={onClose}>
        Done
      </button>
    </div>
  );
}
