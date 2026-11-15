// The Panels section's layout controls: the preset import and export, and the project layout.

import { useRef } from "react";
import { PRESET_MAX_CHARS } from "../layout/presets";
import { Row } from "../components/SettingsRow";

/** What the last layout preset export or import did. */
export type LayoutNotice = { kind: "ok" | "error"; text: string };

/** Project layouts and layout presets, shown under Settings → Panels. */
export type LayoutControls = {
  /** The selected environment's name, or null when none is selected. */
  environment: string | null;
  /** Whether the selected environment uses a layout of its own. */
  projectLayout: boolean;
  notice: LayoutNotice | null;
  /** A valid preset read from a file, waiting to replace the layout that applies. */
  pending: { name: string; replaces: string; missingMods: string[] } | null;
  onConfirmImport: () => void;
  onCancelImport: () => void;
  onSaveProjectLayout: () => void;
  onUseGlobalLayout: () => void;
  onExport: () => void;
  /** Applies a preset file's text, or reports why it was refused through `notice`. */
  onImport: (text: string) => void;
};

/** The project layout and preset rows of Settings → Panels. */
export function LayoutRows({ controls }: { controls: LayoutControls }) {
  const picker = useRef<HTMLInputElement>(null);
  const choose = async (file: File | undefined) => {
    if (!file) return;
    // Layouts are tiny: only enough of a larger file is read for it to be refused as too large.
    controls.onImport(await file.slice(0, PRESET_MAX_CHARS + 1).text());
  };
  return (
    <>
      <Row
        label="Project layout"
        description={
          controls.environment === null
            ? "Select an environment to give it a layout of its own."
            : controls.projectLayout
              ? `${controls.environment} uses its own layout. Changes to panels apply to it only.`
              : `${controls.environment} follows the global layout. Save it as a project layout to arrange its panels separately.`
        }
      >
        {controls.projectLayout ? (
          <button onClick={controls.onUseGlobalLayout}>Use global layout</button>
        ) : (
          <button onClick={controls.onSaveProjectLayout} disabled={controls.environment === null}>
            Save as project layout
          </button>
        )}
      </Row>
      <Row
        label="Layout presets"
        description="Export the layout that applies now as a JSON file to keep or share, or apply one. A preset holds panel places, closed panels and modules, never code."
      >
        <button onClick={controls.onExport}>Export…</button>
        <button onClick={() => picker.current?.click()}>Import…</button>
        <input
          ref={picker}
          type="file"
          accept=".json,application/json"
          hidden
          aria-label="Layout preset file"
          onChange={(event) => {
            void choose(event.target.files?.[0]);
            event.target.value = "";
          }}
        />
      </Row>
      {controls.pending && (
        <Row
          label={`Replace the current layout with “${controls.pending.name}”?`}
          description={
            `This replaces ${controls.pending.replaces}.` +
            (controls.pending.missingMods.length > 0 ? ` It places panels from mods that are not installed: ${controls.pending.missingMods.join(", ")}.` : "")
          }
        >
          <button className="primary" onClick={controls.onConfirmImport}>
            Replace
          </button>
          <button onClick={controls.onCancelImport}>Cancel</button>
        </Row>
      )}
      {controls.notice && (
        <div className={`settings-row-note${controls.notice.kind === "error" ? " error" : ""}`} role={controls.notice.kind === "error" ? "alert" : "status"}>
          {controls.notice.text}
        </div>
      )}
    </>
  );
}
