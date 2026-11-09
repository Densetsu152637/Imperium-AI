// The favourite and hide toggles on a model row, and the collapsed "Hidden" group at the end of a
// model list. The marks themselves are kept in the settings (modelPrefs.ts).

import { ChevronDown, ChevronRight, Eye, EyeOff, Star } from "lucide-react";

import type { ModelMark } from "../modelPrefs";

/** A star (favourite) and an eye (hide / show again) for one model. */
export function ModelMarkButtons({
  name,
  mark,
  onToggle,
}: {
  name: string;
  mark: ModelMark | null;
  onToggle: (mark: ModelMark) => void;
}) {
  const favourite = mark === "favourite";
  const hidden = mark === "hidden";
  return (
    <span className="model-marks">
      <button
        type="button"
        className={`ghost icon model-mark${favourite ? " on" : ""}`}
        aria-pressed={favourite}
        aria-label={`Favourite ${name}`}
        title={favourite ? "Favourite — listed first; click to unfavourite" : "Favourite: list it first when choosing a model"}
        onClick={() => onToggle("favourite")}
      >
        <Star aria-hidden="true" fill={favourite ? "currentColor" : "none"} />
      </button>
      <button
        type="button"
        className={`ghost icon model-mark${hidden ? " on" : ""}`}
        aria-pressed={hidden}
        aria-label={`Hide ${name}`}
        title={hidden ? "Hidden — click to show it again" : "Hide: leave it out of the session model lists"}
        onClick={() => onToggle("hidden")}
      >
        {hidden ? <Eye aria-hidden="true" /> : <EyeOff aria-hidden="true" />}
      </button>
    </span>
  );
}

/** The row that opens and closes the hidden models at the end of a table. */
export function HiddenGroupRow({ count, open, colSpan, onToggle }: { count: number; open: boolean; colSpan: number; onToggle: () => void }) {
  return (
    <tr className="model-hidden-group">
      <td colSpan={colSpan}>
        <button type="button" className="ghost" aria-expanded={open} onClick={onToggle}>
          {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />} Hidden ({count})
        </button>
      </td>
    </tr>
  );
}
